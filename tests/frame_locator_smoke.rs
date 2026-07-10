//! `FrameLocator` smoke test: same-origin iframe-scoped queries.
//!
//! `srcdoc` iframes are same-origin to the parent (their content is parsed in
//! the parent's origin), so the main world can reach their `contentDocument`.
//! Skips if no browser is available.

mod common;

use playwright_cdp::types::AriaRole;

/// Outer page embedding a same-origin `srcdoc` iframe whose content lives in a
/// different document than the top page.
const OUTER: &str = r#"<!DOCTYPE html><html><body>
    <div id="outer">top-level text</div>
    <iframe id="f" srcdoc="<button id='b'>inner</button><input id='i' placeholder='p'><div id='deep'>nested</div>"></iframe>
</body></html>"#;

#[tokio::test]
async fn frame_locator_reads_inner_text_and_clicks() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server(OUTER).await;
    page.goto(&url, None).await.unwrap();

    // The button lives inside the iframe's contentDocument, not the top page.
    let inner = page.frame_locator("#f").locator("#b");
    let text = inner.inner_text().await.unwrap();
    assert_eq!(text, "inner", "inner_text scoped to iframe content");

    // Clicking inside the iframe must target the iframe's element.
    page.frame_locator("#f")
        .locator("#b")
        .click(None)
        .await
        .unwrap();

    // The top-level element is NOT visible through the frame locator's query
    // root: a selector for #outer inside the iframe yields nothing.
    let bad = page.frame_locator("#f").locator("#outer");
    let n = bad.count().await.unwrap();
    assert_eq!(n, 0, "top-level element must not be reachable inside the iframe");

    // Fill works inside the iframe too.
    let input = page.frame_locator("#f").locator("#i");
    input.fill("typed", None).await.unwrap();
    let value = page
        .frame_locator("#f")
        .locator("#i")
        .input_value(None)
        .await
        .unwrap();
    assert_eq!(value, "typed", "fill scoped to iframe input");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn frame_locator_get_by_helpers() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let url = common::http_server(OUTER).await;
    page.goto(&url, None).await.unwrap();

    // get_by_role scoped inside the iframe.
    let role = page.frame_locator("#f").get_by_role(AriaRole::Button, None);
    let t = role.inner_text().await.unwrap();
    assert_eq!(t, "inner");

    // get_by_text scoped inside the iframe finds the nested div.
    let by_text = page.frame_locator("#f").get_by_text("nested", false);
    let n = by_text.count().await.unwrap();
    assert!(n >= 1, "get_by_text should find the nested div inside the iframe");

    // get_by_placeholder scoped inside the iframe.
    let ph = page.frame_locator("#f").get_by_placeholder("p");
    assert_eq!(ph.count().await.unwrap(), 1);

    browser.close().await.unwrap();
}

#[tokio::test]
async fn frame_locator_nested_srcdoc() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    // Outer page -> iframe#outer -> iframe#inner -> button#deep
    let outer = r#"<!DOCTYPE html><html><body>
        <iframe id="outer" srcdoc="<iframe id='inner' srcdoc='<button id=&quot;deep&quot;>deep</button>'></iframe>"></iframe>
    </body></html>"#;
    let url = common::http_server(outer).await;
    page.goto(&url, None).await.unwrap();

    // Give nested srcdoc iframes a moment to parse.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let text = page
        .frame_locator("#outer")
        .frame_locator("#inner")
        .locator("#deep")
        .inner_text()
        .await
        .unwrap();
    assert_eq!(text, "deep", "nested same-origin iframes descend correctly");

    browser.close().await.unwrap();
}
