//! Smoke tests for the `expect` / `expect_page` assertions API. Skips if no
//! browser is available.

mod common;

use std::time::Duration;

use playwright_cdp::{expect, expect_page};

#[tokio::test]
async fn to_be_visible_and_hidden_toggle() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<div id="t" style="display:block">hi</div>"#)
        .await
        .unwrap();

    let div = page.locator("#t");
    expect(div.clone())
        .to_be_visible()
        .await
        .expect("div visible");

    // Hide it and assert hidden.
    let _: serde_json::Value =
        page.evaluate("document.getElementById('t').style.display = 'none'")
            .await
            .unwrap();
    expect(div).to_be_hidden().await.expect("div hidden");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn to_have_text_and_count_and_attribute() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(
        r#"<ul id="list">
            <li class="item" data-idx="1">one</li>
            <li class="item" data-idx="2">two</li>
            <li class="item" data-idx="3">three</li>
        </ul>"#,
    )
    .await
    .unwrap();

    let items = page.locator(".item");
    expect(items.clone()).to_have_count(3).await.expect("3 items");
    expect(page.locator(".item").nth(0))
        .to_have_text("one")
        .await
        .expect("first item text");
    expect(page.locator(".item").nth(1))
        .to_have_attribute("data-idx", "2")
        .await
        .expect("second item attr");

    // to_contain_text
    expect(page.locator(".item").nth(2))
        .to_contain_text("hree")
        .await
        .expect("contains substring");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn to_have_value_and_empty() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<input id="i" value="">"#).await.unwrap();

    let input = page.locator("#i");
    expect(input.clone()).to_be_empty().await.expect("empty input");
    input.fill("hello", None).await.unwrap();
    expect(input).to_have_value("hello").await.expect("value hello");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn to_be_checked_and_unchecked() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<input id="cb" type="checkbox">"#).await.unwrap();

    let cb = page.locator("#cb");
    expect(cb.clone()).to_be_unchecked().await.expect("unchecked");
    cb.check(None).await.unwrap();
    expect(cb).to_be_checked().await.expect("checked");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn page_title_and_url() {
    let url = common::http_server("<!DOCTYPE html><html><head><title>My Page</title></head><body>x</body></html>")
        .await;
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.goto(&url, None).await.unwrap();

    expect_page(page.clone())
        .to_have_title("My Page")
        .await
        .expect("title");
    expect_page(page).to_have_url(&url).await.expect("url");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn negated_to_have_text_succeeds_for_absent() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<div id="t">real value</div>"#).await.unwrap();

    // The text is NOT "absent" — negated assertion should resolve quickly.
    expect(page.locator("#t"))
        .not()
        .to_have_text("absent")
        .await
        .expect("negated text");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn failure_case_times_out() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<div id="t">a</div>"#).await.unwrap();

    // A short timeout so the failure case is fast.
    let result = expect(page.locator("#t"))
        .with_timeout(Duration::from_millis(400))
        .to_have_text("never-matches")
        .await;
    assert!(result.is_err(), "expected timeout error, got {result:?}");

    // Negated assertion that can never hold (text never becomes the wrong
    // value) should also fail.
    let result = expect(page.locator("#t"))
        .with_timeout(Duration::from_millis(400))
        .not()
        .to_have_text("a")
        .await;
    assert!(result.is_err(), "expected timeout error, got {result:?}");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn inner_text_assertion() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<div id="t">Hello&nbsp;World</div>"#).await.unwrap();

    expect(page.locator("#t"))
        .to_have_inner_text("Hello\u{a0}World")
        .await
        .expect("inner text");

    browser.close().await.unwrap();
}
