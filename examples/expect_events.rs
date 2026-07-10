//! Event-waiting helpers: `expect_response`, `expect_event`,
//! `expect_console_message`, and `expect_popup`.
//!
//! Each helper races the wait against the action that triggers it via
//! `tokio::join!`. The page is self-contained: navigation targets come from an
//! in-process HTTP server (copied from `tests/common`), and the console / popup
//! triggers are inline `page.evaluate` scripts — no external site is involved.
//!
//! # Note on `expect_popup`
//!
//! Popup detection is a polling approximation (see `Page::expect_popup` docs):
//! stock Chrome does not surface `window.open` targets through this page's
//! child-target auto-attach, so the helper polls `Target.getTargets` and treats
//! the first new `type == "page"` target in the context as the popup. We use a
//! modest timeout and treat a timeout here as non-fatal (print + move on)
//! rather than `assert!`-ing, so the example stays robust to that approximation.
//!
//! Run: `cargo run --example expect_events`
//! Requires a Chrome/Chromium on the system, or set `PWC_CHROME_PATH`.

use std::time::Duration;

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::Playwright;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A minimal loop-accepting HTTP server that replies `200` with a fixed body
/// for every request. Returns the base URL (with trailing slash). Mirrors the
/// helper in `tests/common/mod.rs`.
async fn http_server(body: &'static str) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { break };
            tokio::spawn(async move {
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

    // Two self-contained HTTP targets so navigation has something real to hit
    // and to wait on (a response, a load event).
    let url1 = http_server("<html><head><title>expect_response target</title></head><body><h1>one</h1></body></html>").await;
    let url2 = http_server("<html><head><title>expect_event target</title></head><body><h1>two</h1></body></html>").await;

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;

    // 1) expect_response: race the wait against the navigation. The URL
    //    predicate "127.0.0.1" matches our local server.
    let (resp, nav1) = tokio::join!(
        page.expect_response("127.0.0.1", Some(Duration::from_secs(5))),
        page.goto(&url1, None),
    );
    nav1?;
    let resp = resp?;
    println!("expect_response matched: status={} url={}", resp.status(), resp.url());
    assert_eq!(resp.status(), 200);

    // 2) expect_event: wait for the page's load event while navigating.
    let (params, nav2) = tokio::join!(
        page.expect_event("Page.loadEventFired", Some(Duration::from_secs(5))),
        page.goto(&url2, None),
    );
    nav2?;
    let _params = params?;
    println!("expect_event matched: Page.loadEventFired (params keys present)");

    // 3) expect_console_message: race the wait against the script that logs.
    let (msg, eval) = tokio::join!(
        page.expect_console_message("hello", Some(Duration::from_secs(5))),
        page.evaluate::<()>("console.log('hello world')"),
    );
    eval?;
    let msg = msg?;
    println!(
        "expect_console_message matched: type={} text={:?}",
        msg.r#type(),
        msg.text()
    );
    assert!(msg.text().contains("hello"));

    // 4) expect_popup: race the wait against window.open. Polling-based, so we
    //    tolerate a timeout rather than crashing the example.
    let (popup, eval) = tokio::join!(
        page.expect_popup(Some(Duration::from_secs(5))),
        page.evaluate::<()>("window.open('about:blank')"),
    );
    eval?;
    match popup {
        Ok(popup) => {
            let popup_url = popup.url().await.unwrap_or_default();
            println!("expect_popup matched: popup url={popup_url}");
            // Drive the popup a little to prove we got a real Page handle.
            popup.close().await.ok();
        }
        Err(e) => {
            // Popup detection is a polling approximation; a miss is expected to
            // be possible rather than a hard failure.
            println!("expect_popup did not match within timeout (polling approximation): {e}");
        }
    }

    println!("all event-waiting demos completed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
