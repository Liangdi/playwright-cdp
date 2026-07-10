//! Network capture + request routing/interception.
//!
//! Spins a tiny local HTTP server, registers request/response listeners so
//! every network event is printed, then exercises the three routing behaviors:
//! `fulfill` (mock a response), `abort` (block a request), and the default
//! pass-through capture of the document response.
//!
//! Run: `cargo run --example network`

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::route::RouteFulfillOptions;
use playwright_cdp::Playwright;

/// A minimal loop-accepting HTTP server that replies `200` with a fixed body
/// for every request. Returns the base URL (with trailing slash).
async fn serve(body: &'static str) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { break };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
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

    // A page with a visible heading so we can sanity-check the captured body.
    let html = "<!doctype html><html><head><title>Network Demo</title></head>\
                <body><h1 id=\"h\">network capture works</h1></body></html>";
    let base = serve(html).await;
    println!("serving demo page at {base}");

    let page = browser.new_page().await?;

    // --- capture: register listeners BEFORE navigating ---
    page.on_request(|req| async move {
        println!(">> {} {}", req.method(), req.url());
    });
    page.on_response(|resp| async move {
        println!("<< {} {}", resp.status(), resp.url());
    });

    println!("navigating to {base} (capture listeners armed)...");
    let resp = page.goto(&base, None).await?.expect("document response");
    println!("--- captured document response ---");
    println!("status: {}  ok: {}", resp.status(), resp.ok());
    println!("content-type: {:?}", resp.headers().get("content-type"));
    let body = resp.text().await?;
    let snippet: String = body.chars().take(80).collect();
    println!("body (first ~80 chars): {snippet}");
    assert!(resp.ok());
    assert!(body.contains("network capture works"));

    // --- routing: fulfill a mocked response ---
    page.route("*mocked*", |route| async move {
        route
            .fulfill(
                RouteFulfillOptions::new()
                    .status(200)
                    .content_type("text/html")
                    .body("<h1 id=\"m\">mocked-by-route</h1>"),
            )
            .await
            .ok();
    })
    .await?;
    println!("\nnavigating to {base}mocked (route will fulfill)...");
    page.goto(&format!("{base}mocked"), None).await?;
    let mocked: String = page
        .evaluate("document.getElementById('m').textContent")
        .await?;
    println!("route-fulfilled element text: {mocked}");
    assert_eq!(mocked, "mocked-by-route");

    // --- routing: abort (block) a request ---
    page.route("*blocked*", |route| async move {
        route.abort(None).await.ok();
    })
    .await?;
    println!("\nnavigating to {base}blocked (route will abort)...");
    if let Err(e) = page.goto(&format!("{base}blocked"), None).await {
        println!("blocked nav errored (expected): {e}");
    }

    println!("\nall network capture + routing steps completed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
