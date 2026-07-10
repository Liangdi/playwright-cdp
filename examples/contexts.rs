//! Browser-context isolation + cookies + storage_state round-trip.
//!
//! Two contexts share the same browser but have isolated storage. We set a
//! cookie and a localStorage entry in context #1, snapshot its storage state,
//! restore it into context #2, and confirm both the cookie and the localStorage
//! value survive the round-trip.
//!
//! Run: `cargo run --example contexts`

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::types::Cookie;
use playwright_cdp::Playwright;

/// A minimal loop-accepting HTTP server serving a fixed HTML page so we have a
/// real, stable origin for cookies and localStorage. Returns the base URL.
async fn serve() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { break };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let body = "<!doctype html><html><body><h1>contexts demo</h1></body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.flush().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let url = serve().await;
    println!("serving contexts demo page at {url}");

    // --- context #1: set a cookie + localStorage, then snapshot ---
    let ctx1 = browser.new_context().await?;
    let p1 = ctx1.new_page().await?;
    p1.goto(&url, None).await?;

    // `Cookie` does not derive Default, so construct it explicitly with the
    // fields we care about and `None` for the rest.
    ctx1.add_cookies(&[Cookie {
        name: "session".into(),
        value: "abc".into(),
        url: Some(url.clone()),
        domain: None,
        path: None,
        expires: None,
        http_only: None,
        secure: None,
        same_site: None,
    }])
    .await?;
    println!("added cookie session=abc to context #1");

    // Reload so the new cookie is present in document.cookie.
    p1.reload(None).await?;
    let doc_cookie: String = p1.evaluate("document.cookie").await?;
    println!("document.cookie after reload: {doc_cookie}");
    assert!(doc_cookie.contains("session=abc"));

    p1.evaluate::<()>("localStorage.setItem('k','v')").await?;
    println!("set localStorage k=v in context #1");

    let state = ctx1.storage_state().await?;
    println!(
        "\nsnapshot: cookies={}  origins={}",
        state.cookies.len(),
        state.origins.len()
    );
    for o in &state.origins {
        let names: Vec<&str> = o.localStorage.iter().map(|nv| nv.name.as_str()).collect();
        println!("  origin {} localStorage keys: {:?}", o.origin, names);
    }
    assert!(
        state.cookies.iter().any(|c| {
            c.get("name").and_then(|v| v.as_str()) == Some("session")
                && c.get("value").and_then(|v| v.as_str()) == Some("abc")
        }),
        "snapshot should contain the session cookie"
    );

    // --- context #2: restore the snapshot, confirm cookie + localStorage ---
    let ctx2 = browser.new_context().await?;
    ctx2.set_storage_state(&state).await?;
    let p2 = ctx2.new_page().await?;
    p2.goto(&url, None).await?;

    let ls: String = p2.evaluate("localStorage.getItem('k')").await?;
    println!("\ncontext #2 localStorage.getItem('k'): {ls:?}");
    assert_eq!(ls, "v");

    let doc_cookie2: String = p2.evaluate("document.cookie").await?;
    println!("context #2 document.cookie: {doc_cookie2}");
    assert!(
        doc_cookie2.contains("session=abc"),
        "restored cookie should be visible in context #2"
    );

    println!("\ncontext isolation + storage_state round-trip verified.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
