//! Route interception: `Route::fetch` (real request inside the handler) + a
//! `**` URL glob pattern.
//!
//! `page.route("**/api/**", handler)` matches any URL containing an `/api/`
//! segment (the `**` glob crosses path separators). Inside the handler,
//! `route.fetch(None)` issues a **real** HTTP request that bypasses
//! interception, captures the response, and `route.fulfill(...)` re-serves it
//! to the page.
//!
//! Run: `cargo run --example route_fetch`
//! Requires a Chrome/Chromium on the system, or set `CHROME_PATH`.

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::route::{RouteFulfillOptions, Route};
use playwright_cdp::Playwright;

/// A tiny loop-accepting HTTP server that serves JSON `{"server":"real"}` for
/// every connection. Stays up until the runtime drops the task. Returns its
/// base URL. Mirrors `tests/common::http_server`.
async fn http_server(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let body = body;
            // Per-connection task so a stalled connection (e.g. a request held
            // by Fetch.requestPaused) can't block route.fetch's own request.
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
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

    let base = http_server(r#"{"server":"real"}"#).await;
    let target = format!("{base}api/data");
    println!("embedded JSON server: {base}");
    println!("target URL (matches **/api/**): {target}");

    let page = browser.new_page().await?;

    // Register the route. The pattern uses the `**` glob: `**/api/**` matches
    // any URL whose path contains an `/api/` segment, regardless of host.
    println!("\nregistering route with pattern `**/api/**`");
    page.route("**/api/**", |route: Route| async move {
        // `route.fetch` issues a REAL request via a standalone reqwest client
        // that bypasses CDP interception — it does not re-trigger this handler.
        println!("  [handler] route.fetch -> real request");
        let resp = route.fetch(None).await.expect("fetch should succeed");
        println!("  [handler] fetched status: {}", resp.status());
        let text = resp.text();
        println!("  [handler] fetched body: {text}");
        assert!(text.contains(r#""server":"real""#), "fetch hit the real server");

        // Re-serve the fetched body to the page via fulfill.
        route
            .fulfill(
                RouteFulfillOptions::new()
                    .status(200)
                    .content_type("application/json")
                    .body(text),
            )
            .await
            .ok();
    })
    .await?;

    // Navigate to a URL matching `**/api/**`.
    println!("\n[page] goto {target}");
    page.goto(&target, None).await?;

    // The page should render the fulfilled JSON body (proving fetch + fulfill
    // worked end-to-end).
    let body_text: String = page
        .evaluate("document.body && document.body.textContent")
        .await?;
    println!("[page] body.textContent: {body_text}");
    assert!(
        body_text.contains(r#""server":"real""#),
        "page should show the fulfilled content, got: {body_text}"
    );

    println!("\nroute.fetch + fulfill served the real server's body to the page.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
