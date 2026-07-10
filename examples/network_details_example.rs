//! Network detail demo: exercises the new `Request`/`Response` accessors
//! (`post_data`, `http_version`, `server_addr`, …) by issuing a same-origin
//! POST and capturing the request/response via `on_request`/`on_response`.
//!
//! Run: `cargo run --example network_details_example`

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::{Playwright, Request, Response};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    let base = http_server_echo_post().await;

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    // Same-origin navigation so the subsequent fetch isn't CORS-blocked.
    page.goto(&base, None).await?;

    let captured_req = Arc::new(Mutex::new(None::<Request>));
    let captured_res = Arc::new(Mutex::new(None::<Response>));

    let crq = Arc::clone(&captured_req);
    page.on_request(move |r| {
        let c = Arc::clone(&crq);
        async move {
            if r.url().ends_with("/post") {
                *c.lock().await = Some(r);
            }
        }
    });
    let cres = Arc::clone(&captured_res);
    page.on_response(move |r| {
        let c = Arc::clone(&cres);
        async move {
            if r.url().ends_with("/post") {
                *c.lock().await = Some(r);
            }
        }
    });

    // Issue a POST with a body (IIFE so page.evaluate wraps it as a returned
    // promise that awaitPromise resolves).
    let status: i64 = page
        .evaluate(
            r#"(async () => { const r = await fetch("/post", { method: "POST", body: "payload-123" }); return r.status; })()"#,
        )
        .await?;
    assert_eq!(status, 200, "POST should return 200");

    // Let the response event land.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let req = captured_req
        .lock()
        .await
        .clone()
        .expect("captured POST request");
    assert_eq!(
        req.post_data(),
        Some("payload-123".to_string()),
        "request.post_data() should capture the POST body"
    );
    println!("request.post_data() = {:?}", req.post_data());
    println!("request.all_headers() keys: {:?}", req.all_headers().keys().collect::<Vec<_>>());

    let res = captured_res
        .lock()
        .await
        .clone()
        .expect("captured POST response");
    assert_eq!(res.status(), 200);
    println!(
        "response.status={} http_version={} server_addr={:?}",
        res.status(),
        res.http_version(),
        res.server_addr()
    );

    println!("PASS");
    browser.close().await?;
    Ok(())
}

/// Tiny HTTP server that echoes the POST body as `echo:<body>`. Used to give
/// the page a same-origin POST target.
async fn http_server_echo_post() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                let resp_body = format!("echo:{body}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}
