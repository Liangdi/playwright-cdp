//! End-to-end demo of the `WebStorage` (localStorage / sessionStorage) API.
//!
//! DOMStorage requires a real http origin (not `about:blank` / `data:`), so we
//! stand up a tiny one-shot local HTTP server, navigate to it, and then exercise
//! both stores.
//!
//! Run: `cargo run --example web_storage_example`
//! Requires a Chrome/Chromium on the system, or set `CHROME_PATH`.

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    // DOMStorage needs a real http origin. Stand up a tiny server and navigate
    // to it before touching storage.
    let base = http_server("<html><body>storage test</body></html>").await;

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    page.goto(&base, None).await?;

    // --- localStorage ---
    let ls = page.local_storage().await;
    println!("localStorage origin: {}", ls.security_origin());

    ls.set("foo", "bar").await?;
    assert_eq!(ls.get("foo").await?, Some("bar".to_string()));
    println!("localStorage set/get foo=bar OK");

    ls.set("count", "42").await?;
    let size = ls.size().await?;
    let keys = ls.keys().await?;
    assert!(size >= 2, "expected at least 2 entries, got {size}");
    assert!(ls.has("foo").await?, "expected 'foo' to be present");
    println!("localStorage size={size} keys={keys:?} has(foo)=true OK");

    ls.remove("foo").await?;
    assert_eq!(ls.get("foo").await?, None);
    println!("localStorage remove(foo) -> get None OK");

    // --- sessionStorage ---
    let ss = page.session_storage().await;
    println!("sessionStorage origin: {}", ss.security_origin());

    ss.set("temp", "session-value").await?;
    assert_eq!(
        ss.get("temp").await?,
        Some("session-value".to_string())
    );
    assert!(ss.has("temp").await?, "expected sessionStorage 'temp'");
    println!("sessionStorage set/get temp=session-value OK");

    // --- clear localStorage ---
    ls.clear().await?;
    let cleared = ls.size().await?;
    assert_eq!(cleared, 0, "localStorage should be empty after clear, got {cleared}");
    println!("localStorage clear() -> size=0 OK");

    println!("PASS");
    browser.close().await?;
    Ok(())
}

/// Bind a tiny single-route HTTP server to an ephemeral port and return its
/// base URL. Serves `body` for every request, forever.
async fn http_server(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    format!("http://127.0.0.1:{port}/")
}
