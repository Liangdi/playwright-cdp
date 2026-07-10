//! Standalone HTTP client configured via `ApiRequestContextOptions`.
//!
//! `APIRequestContext::new_with_options(opts)` builds a direct HTTP client
//! (backed by reqwest, no browser involved) from a single options struct:
//! `base_url`, `user_agent`, `timeout`, `ignore_https_errors`, and
//! `extra_http_headers`. Relative request URLs are joined onto `base_url`;
//! absolute URLs are used verbatim.
//!
//! Run: `cargo run --example api_request_config`
//! (No browser required — this is a pure HTTP client, but a Chrome path env
//! isn't needed.)

use std::collections::HashMap;
use std::time::Duration;

use playwright_cdp::api_request::ApiRequestContextOptions;
use playwright_cdp::APIRequestContext;

/// A tiny loop-accepting HTTP server that serves `body` at `/` for every
/// connection. Stays up until the runtime drops the task. Returns its base URL.
/// Mirrors `tests/common::http_server`.
async fn http_server(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let body = body;
            tokio::spawn(async move {
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

    let base = http_server("<api>config</api>").await;
    println!("embedded server base: {base}");

    // Build the client from a single options struct. Note the chained builder
    // setters: base_url, user_agent, timeout, ignore_https_errors, and an
    // extra custom header.
    let mut extras: HashMap<String, String> = HashMap::new();
    extras.insert("x-demo-header".to_string(), "hello".to_string());

    let ctx = APIRequestContext::new_with_options(
        ApiRequestContextOptions::new()
            .base_url(base.trim_end_matches('/'))
            .user_agent("demo-agent/1.0")
            .timeout(Duration::from_secs(10))
            .ignore_https_errors(true)
            .extra_http_headers(extras),
    );
    println!("built APIRequestContext with base_url + user_agent + timeout + extra headers");

    // default_headers() exposes the seeded extra_http_headers.
    assert_eq!(
        ctx.default_headers().get("x-demo-header"),
        Some(&"hello".to_string()),
    );

    // --- Relative path "/" joins onto base_url ---
    println!("\n[ctx] GET \"/\"  (relative -> joined to base_url)");
    let resp = ctx.get("/", None).await?;
    println!("  status: {}  ok: {}", resp.status(), resp.ok());
    println!("  final url: {}", resp.url());
    let body = resp.text().await?;
    println!("  body: {body}");
    assert_eq!(resp.status(), 200);
    assert!(body.contains("config"));
    // The resolved URL should carry the embedded server's port.
    assert!(resp.url().starts_with("http://127.0.0.1:"));

    // --- Absolute URL wins over base_url (no override) ---
    // A second server proves an absolute URL is used verbatim and base_url
    // is NOT prepended.
    let other = http_server("<api>absolute</api>").await;
    println!("\n[ctx] GET <absolute url>  (base_url ignored)");
    let resp = ctx.get(&other, None).await?;
    println!("  status: {}  ok: {}", resp.status(), resp.ok());
    println!("  final url: {}", resp.url());
    let body = resp.text().await?;
    println!("  body: {body}");
    assert_eq!(resp.status(), 200);
    assert!(body.contains("absolute"));
    assert!(resp.url().starts_with(&other));

    println!("\nApiRequestContextOptions (base_url / user_agent / timeout / ignore_https_errors / extra_http_headers) all applied.");
    println!("done.");
    Ok(())
}
