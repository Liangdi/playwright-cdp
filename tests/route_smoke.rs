//! Route interception + PDF smoke tests.

mod common;

use playwright_cdp::options::PaperFormat;
use playwright_cdp::route::RouteFulfillOptions;

#[tokio::test]
async fn route_fulfill_serves_mock_response() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // The real server serves this body for any path; the route will override it.
    let base = common::http_server("SHOULD-NOT-APPEAR").await;
    let mocked = format!("{base}mocked"); // http://127.0.0.1:PORT/mocked

    page.route("*mocked*", |route| async move {
        route
            .fulfill(
                RouteFulfillOptions::new()
                    .status(200)
                    .content_type("text/html")
                    .body("<html><body><h1 id=m>mocked</h1></body></html>"),
            )
            .await
            .ok();
    })
    .await
    .unwrap();

    page.goto(&mocked, None).await.unwrap();
    let h1: String = page
        .evaluate("document.getElementById('m') && document.getElementById('m').textContent")
        .await
        .unwrap();
    assert_eq!(h1, "mocked");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn route_abort_blocks_request() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server("<p>real</p>").await;

    page.route("*blocked*", |route| async move {
        route.abort(None).await.ok();
    })
    .await
    .unwrap();

    let blocked = format!("{url}blocked");
    // Aborted navigation yields an error from goto.
    let result = page.goto(&blocked, None).await;
    assert!(result.is_err(), "aborted navigation should error");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn pdf_returns_bytes() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content("<html><body><h1>print me</h1></body></html>")
        .await
        .unwrap();
    let pdf = page
        .pdf(Some(playwright_cdp::options::PdfOptions::new().format(PaperFormat::A4)))
        .await
        .unwrap();
    // PDFs start with "%PDF".
    assert!(pdf.len() > 100, "pdf too small: {} bytes", pdf.len());
    assert_eq!(&pdf[..4], b"%PDF");
    browser.close().await.unwrap();
}
