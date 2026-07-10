//! Context-level events: a single handler registered on the `BrowserContext`
//! fans out to **every** page in that context.
//!
//! This proves that `context.on_request` / `context.on_response` fire for any
//! page — we register them *before* creating any pages, open two pages, and
//! confirm the one shared handler observed traffic from both.
//!
//! Run: `cargo run --example context_events_example`

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::Playwright;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A minimal loop-accepting HTTP server that replies `200` with a fixed body
/// for every request. Returns the base URL (with trailing slash).
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

    // A distinct page body per page so we can confirm both pages actually loaded.
    let base1 = http_server("<html><body><h1 id=\"a\">page one</h1></body></html>").await;
    let base2 = http_server("<html><body><h1 id=\"b\">page two</h1></body></html>").await;

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    // 1) An isolated context. Context-level handlers will fan out to every page
    //    created within it.
    let context = browser.new_context().await?;

    // 2) Counters shared with the handlers (cloned into each closure).
    //    - req_count: total requests seen by the single context handler.
    //    - first_req_url: the very first request URL captured (proves the
    //      handler fired at all, and before we touch the pages ourselves).
    //    - first_resp_status: status of the first response captured.
    let req_count = Arc::new(AtomicU64::new(0));
    let first_req_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let first_resp_status: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));

    // Register the context handlers BEFORE creating any pages. If the fan-out
    // wiring is correct, these still fire for pages created afterwards.
    {
        let req_count = Arc::clone(&req_count);
        let first_req_url = Arc::clone(&first_req_url);
        context.on_request(move |req| {
            let n = req_count.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                // Capture the first request URL exactly once.
                *first_req_url.lock().unwrap() = Some(req.url().to_string());
            }
            println!("[ctx] request  #{}  {} {}", n, req.method(), req.url());
            async {}
        });
    }
    {
        let first_resp_status = Arc::clone(&first_resp_status);
        context.on_response(move |resp| {
            let mut slot = first_resp_status.lock().unwrap();
            if slot.is_none() {
                *slot = Some(resp.status());
            }
            println!("[ctx] response    {} {}", resp.status(), resp.url());
            async {}
        });
    }

    // 3) First page in the context.
    let page1 = context.new_page().await?;
    // 4) Navigate it.
    page1.goto(&base1, None).await?;
    let h1: String = page1.evaluate("document.getElementById('a').textContent").await?;
    println!("page1 heading: {h1}");
    assert_eq!(h1, "page one");

    // 5) A SECOND page in the same context, navigated to a different URL. The
    //    single context-level handler must also capture this page's traffic.
    let page2 = context.new_page().await?;
    page2.goto(&base2, None).await?;
    let h2: String = page2.evaluate("document.getElementById('b').textContent").await?;
    println!("page2 heading: {h2}");
    assert_eq!(h2, "page two");

    // 6) Let the in-flight network events settle.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let total = req_count.load(Ordering::SeqCst);
    println!("total requests captured by the single context handler: {total}");

    // The context handler saw at least one request per page (the two document
    // navigations), proving the fan-out covered both pages.
    assert!(
        total >= 2,
        "context handler should have seen >=2 requests (one per page), got {total}"
    );

    // The first captured request URL should be page1's base URL.
    let first_url = first_req_url.lock().unwrap().clone();
    println!("first captured request url: {:?}", first_url);
    assert_eq!(first_url.as_deref(), Some(base1.as_str()));

    // 7) The context handler captured at least one response with a sensible
    //    status (200 from our local server).
    let status = first_resp_status.lock().unwrap();
    println!("first captured response status: {:?}", status);
    assert_eq!(*status, Some(200));

    println!("PASS");
    browser.close().await?;
    Ok(())
}
