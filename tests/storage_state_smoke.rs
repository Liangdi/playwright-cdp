//! `BrowserContext::storage_state` / `set_storage_state` smoke tests.

mod common;

use playwright_cdp::types::Cookie;

#[tokio::test]
async fn storage_state_roundtrip_cookies_and_localstorage() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };

    let url = common::http_server("<html><body>storage</body></html>").await;

    // --- Context 1: set localStorage + a cookie, then snapshot ---
    let context1 = browser.new_context().await.unwrap();
    let page1 = context1.new_page().await.unwrap();
    page1.goto(&url, None).await.unwrap();

    // Write a localStorage entry on the page.
    let _: serde_json::Value = page1
        .evaluate("localStorage.setItem('k', 'v')")
        .await
        .unwrap();

    // Add a cookie for this origin.
    context1
        .add_cookies(&[Cookie {
            name: "ctest".to_string(),
            value: "cv".to_string(),
            url: Some(url.clone()),
            domain: None,
            path: None,
            expires: None,
            http_only: None,
            secure: None,
            same_site: None,
        }])
        .await
        .unwrap();

    let state = context1.storage_state().await.unwrap();

    // Cookies non-empty.
    assert!(!state.cookies.is_empty(), "expected cookies in state");

    // An origin entry whose localStorage has {k: v}.
    let entry = state
        .origins
        .iter()
        .find(|o| o.localStorage.iter().any(|nv| nv.name == "k"));
    let entry = entry.expect("expected an origin with localStorage entry 'k'");
    let kv = entry
        .localStorage
        .iter()
        .find(|nv| nv.name == "k")
        .expect("expected 'k' entry");
    assert_eq!(kv.value, "v", "expected localStorage['k'] == 'v'");

    // --- Context 2: restore state, then read it back ---
    let context2 = browser.new_context().await.unwrap();
    context2.set_storage_state(&state).await.unwrap();

    // A new page at the same origin shares localStorage (same-origin).
    let page2 = context2.new_page().await.unwrap();
    page2.goto(&url, None).await.unwrap();

    let val: String = page2
        .evaluate("localStorage.getItem('k')")
        .await
        .unwrap();
    assert_eq!(val, "v", "expected restored localStorage['k'] == 'v'");

    // Cookie should be present too.
    let cookies2 = context2.cookies().await.unwrap();
    let has_cookie = cookies2
        .iter()
        .any(|c| c.get("name").and_then(|v| v.as_str()) == Some("ctest"));
    assert!(has_cookie, "expected restored cookie 'ctest' present");

    browser.close().await.unwrap();
}
