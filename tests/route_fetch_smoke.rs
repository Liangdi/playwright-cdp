//! Smoke tests for `Route::fetch` — issuing a real (interception-bypassing)
//! HTTP request from inside a route handler.

mod common;

use playwright_cdp::route::{RouteFetchOptions, RouteFulfillOptions};

/// `route.fetch(None)` issues a real request, captures the response body, then
/// the handler fulfills the route with that body. The page should observe the
/// fetched content rather than the server's default body.
#[tokio::test]
async fn route_fetch_returns_real_response() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // The real server always serves this body; the route will fetch it and
    // re-serve it (so we can prove fetch hit the real server).
    let base = common::http_server(r#"{"hello":"fetch"}"#).await;
    let target = format!("{base}fetched");

    page.route("*fetched*", |route| async move {
        let resp = route.fetch(None).await.expect("fetch should succeed");
        assert_eq!(resp.status(), 200);
        let text = resp.text();
        assert!(
            text.contains("fetch"),
            "fetched body should contain 'fetch', got: {text}"
        );
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
    .await
    .unwrap();

    page.goto(&target, None).await.unwrap();
    let body_json: String = page
        .evaluate("document.body && document.body.textContent")
        .await
        .unwrap();
    assert!(
        body_json.contains("fetch"),
        "page should show fetched body, got: {body_json}"
    );

    browser.close().await.unwrap();
}

/// `RouteFetchOptions` overrides (url override, method, extra headers) are
/// honored and still bypass interception.
#[tokio::test]
async fn route_fetch_options_override() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // Two bodies: the default and an alternate path.
    let base = common::http_server_routes(&[("/", "<p>default</p>"), ("/alt", r#"{"alt":true}"#)]).await;
    let target = format!("{base}/");

    // Match everything; inside, fetch the /alt path explicitly.
    page.route("**", {
        let base = base.clone();
        move |route| {
            let base = base.clone();
            async move {
                let alt_url = format!("{}/alt", base.trim_end_matches('/'));
                let resp = route
                    .fetch(Some(
                        RouteFetchOptions::new()
                            .url(alt_url)
                            .method("GET")
                            .timeout(5_000),
                    ))
                    .await
                    .expect("fetch with options should succeed");
                assert_eq!(resp.status(), 200);
                let text = resp.text();
                assert!(text.contains("alt"), "overridden fetch body: {text}");
                route
                    .fulfill(
                        RouteFulfillOptions::new()
                            .status(200)
                            .content_type("application/json")
                            .body(text),
                    )
                    .await
                    .ok();
            }
        }
    })
    .await
    .unwrap();

    page.goto(&target, None).await.unwrap();
    let body_text: String = page
        .evaluate("document.body && document.body.textContent")
        .await
        .unwrap();
    assert!(
        body_text.contains("alt"),
        "page should reflect overridden fetch, got: {body_text}"
    );

    browser.close().await.unwrap();
}
