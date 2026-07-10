//! Standalone HTTP client via `page.request()` / `context.request()`.
//!
//! Demonstrates the `APIRequestContext` — a direct HTTP client (backed by
//! reqwest, NOT browser `fetch`) obtained from a page or a browser context.
//! It spins a tiny local server that serves HTML at `/` and JSON at `/json`,
//! then exercises GET, POST-with-JSON, `.json()` parsing, and the
//! context-level client.
//!
//! Run: `cargo run --example api_request`

use playwright_cdp::options::{APIRequestOptions, LaunchOptions};
use playwright_cdp::Playwright;

/// A minimal loop-accepting HTTP server that serves different bodies per path:
/// HTML at `/` and JSON `{"ok":true}` at `/json`. Returns the base URL.
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
                let req_line = String::from_utf8_lossy(&buf);
                let path = req_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let (ctype, body) = if path.starts_with("/json") {
                    ("application/json", "{\"ok\":true}")
                } else {
                    ("text/html", "<h1>api-request demo</h1>")
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    let base = serve().await;
    let json_url = format!("{base}json");
    println!("serving HTML at {base} and JSON at {json_url}");

    // `page.request()` returns a standalone HTTP client; it shares no state
    // with the page — it makes direct reqwest calls, not browser `fetch`.
    // Use an explicit context so we can also demo the context-level client.
    let context = browser.new_context().await?;
    let page = context.new_page().await?;
    let api = page.request();

    // --- GET the HTML page ---
    println!("\n[page.request] GET {base}");
    let r = api.get(&base, None).await?;
    let text = r.text().await?;
    let snippet: String = text.chars().take(60).collect();
    println!("  status: {}  ok: {}", r.status(), r.ok());
    println!("  body: {snippet}");
    assert!(r.ok());
    assert!(text.contains("api-request demo"));

    // --- POST with a JSON body (server echoes a 200 regardless) ---
    println!("\n[page.request] POST {base} with JSON body");
    let r = api
        .post(
            &base,
            Some(APIRequestOptions::new().data(serde_json::json!({"hello":"world"}))),
        )
        .await?;
    println!("  status: {}  ok: {}", r.status(), r.ok());
    assert!(r.ok());

    // --- GET the JSON endpoint and parse it ---
    println!("\n[page.request] GET {json_url} and parse JSON");
    let r = api.get(&json_url, None).await?;
    println!("  status: {}  ok: {}", r.status(), r.ok());
    let parsed = r.json().await?;
    println!("  parsed json: {parsed}");
    assert_eq!(parsed["ok"], serde_json::json!(true));

    // --- context-level client works too ---
    // `context.request()` is the same client shape, seeded (best-effort) with
    // the context's extra HTTP headers. Demonstrate it with a GET.
    println!("\n[context.request] GET {base} (context-level client)");
    let r = context.request().get(&base, None).await?;
    println!("  status: {}  ok: {}", r.status(), r.ok());
    assert!(r.ok());

    println!("\nall api-request steps completed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
