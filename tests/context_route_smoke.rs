//! Context-level routing (`BrowserContext::route`) smoke tests.

mod common;

use playwright_cdp::route::RouteFulfillOptions;

#[tokio::test]
async fn context_route_aborts_on_new_page() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let context = browser.new_context().await.unwrap();
    let url = common::http_server("<p>real</p>").await;

    // Register the context route BEFORE creating the page, to exercise the
    // new-page application path.
    context
        .route("*blocked*", |route| async move {
            route.abort(None).await.ok();
        })
        .await
        .unwrap();

    let blocked = format!("{url}blocked");
    let page1 = context.new_page().await.unwrap();
    // Aborted navigation yields an error from goto.
    let result = page1.goto(&blocked, None).await;
    assert!(result.is_err(), "aborted navigation should error");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn context_route_fulfills_on_new_page() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let context = browser.new_context().await.unwrap();
    let url = common::http_server("SHOULD-NOT-APPEAR").await;

    // Register the context route BEFORE creating the page.
    context
        .route("*mocked*", |route| async move {
            route
                .fulfill(
                    RouteFulfillOptions::new()
                        .status(200)
                        .content_type("text/html")
                        .body("<h1 id=m>ctx</h1>"),
                )
                .await
                .ok();
        })
        .await
        .unwrap();

    let page2 = context.new_page().await.unwrap();
    page2
        .goto(&format!("{url}mocked"), None)
        .await
        .unwrap();

    let m: String = page2
        .evaluate("document.getElementById('m').textContent")
        .await
        .unwrap();
    assert_eq!(m, "ctx");

    browser.close().await.unwrap();
}
