//! Phase B smoke test: locator actions (check/uncheck, select_option, fill,
//! dblclick, bounding_box, get_by_text). Skips if no browser.

mod common;

use playwright_cdp::options::SelectOption;

#[tokio::test]
async fn click_triggers_navigation() {
    // Regression for the buttons-bitmask bug: clicking a link must navigate.
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(
        r#"<a id="go" href="https://example.com">go</a><div id="x">stay</div>"#,
    )
    .await
    .unwrap();
    page.locator("#go").click(None).await.unwrap();
    // wait for the navigation to take effect
    page.wait_for_url("*example.com*", None).await.unwrap();
    let url: String = page.evaluate("location.href").await.unwrap();
    assert!(url.contains("example.com"), "navigated url was {url}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn locator_actions() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let html = r#"<!DOCTYPE html><html><body>
        <input id="cb" type="checkbox">
        <input id="txt" value="">
        <select id="sel"><option value="a">A</option><option value="b">B</option></select>
        <button id="btn">Click me</button>
        <div id="counter">0</div>
        <p>hello world</p>
        <script>
            let n = 0;
            document.getElementById('btn').addEventListener('dblclick', () => {
                n += 1;
                document.getElementById('counter').textContent = n;
            });
        </script>
    </body></html>"#;
    page.set_content(html).await.unwrap();

    // check / uncheck / is_checked
    let cb = page.locator("#cb");
    cb.check(None).await.unwrap();
    assert!(cb.is_checked().await.unwrap());
    cb.uncheck(None).await.unwrap();
    assert!(!cb.is_checked().await.unwrap());

    // fill / input_value
    let txt = page.locator("#txt");
    txt.fill("typed", None).await.unwrap();
    assert_eq!(txt.input_value(None).await.unwrap(), "typed");

    // select_option
    let sel = page.locator("#sel");
    let chosen = sel.select_option(SelectOption::value("b"), None).await.unwrap();
    assert_eq!(chosen, vec!["b".to_string()]);

    // dblclick triggers handler
    page.locator("#btn").dblclick(None).await.unwrap();
    let counter: String = page.evaluate("document.getElementById('counter').textContent").await.unwrap();
    assert_eq!(counter, "1");

    // bounding_box
    let bb = page.locator("#btn").bounding_box().await.unwrap().unwrap();
    assert!(bb.width > 0.0 && bb.height > 0.0);

    // get_by_text
    let p = page.get_by_text("hello world", false);
    let t = p.text_content().await.unwrap();
    assert_eq!(t.as_deref(), Some("hello world"));

    // query_selector / query_selector_all / ElementHandle
    let btn = page.query_selector("#btn").await.unwrap().expect("btn handle");
    assert_eq!(btn.inner_text().await.unwrap(), "Click me");
    let inputs = page.query_selector_all("input").await.unwrap();
    assert_eq!(inputs.len(), 2);
    btn.dispose().await.unwrap();

    browser.close().await.unwrap();
}
