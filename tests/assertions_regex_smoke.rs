//! Smoke tests for the newly added assertion methods: regex variants, CSS,
//! class, focus, value-regex, page-title/url regex, and aria-snapshot. Skips
//! gracefully if no browser is available.

mod common;

use std::time::Duration;

use playwright_cdp::{expect, expect_page};

/// Short timeout so a failing assertion fails the test quickly instead of
/// hanging for the default 5s.
fn short() -> Duration {
    Duration::from_millis(1500)
}

macro_rules! skip_if_none {
    ($b:expr) => {
        match $b {
            Some(b) => b,
            None => {
                eprintln!("[skip] no browser available");
                return;
            }
        }
    };
}

#[tokio::test]
async fn regex_text_inner_text_and_attribute() {
    let browser = skip_if_none!(common::browser().await);
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<div id="t" data-code="abc123">Hello World</div>"#)
        .await
        .unwrap();
    let div = page.locator("#t");
    expect(div.clone())
        .with_timeout(short())
        .to_have_text_regex("Hello")
        .await
        .expect("to_have_text_regex");
    expect(div.clone())
        .with_timeout(short())
        .to_have_inner_text_regex("(?i)hello")
        .await
        .expect("to_have_inner_text_regex");
    expect(div.clone())
        .with_timeout(short())
        .to_have_attribute_regex("data-code", r"\d+")
        .await
        .expect("to_have_attribute_regex");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn css_and_class_assertions() {
    let browser = skip_if_none!(common::browser().await);
    let page = browser.new_page().await.unwrap();
    page.set_content(
        r#"<div id="t" class="btn primary" style="display: block; color: red">x</div>"#,
    )
    .await
    .unwrap();
    let div = page.locator("#t");
    expect(div.clone())
        .with_timeout(short())
        .to_have_css("display", "block")
        .await
        .expect("to_have_css display");
    expect(div.clone())
        .with_timeout(short())
        .to_have_css_regex("color", r"rgb\(255,\s*0,\s*0\)")
        .await
        .expect("to_have_css_regex color");
    expect(div.clone())
        .with_timeout(short())
        .to_have_class("btn primary")
        .await
        .expect("to_have_class exact");
    expect(div.clone())
        .with_timeout(short())
        .to_have_class_regex(r"\bprimary\b")
        .await
        .expect("to_have_class_regex");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn focused_and_value_regex() {
    let browser = skip_if_none!(common::browser().await);
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<input id="i" value="user42@mail.com">"#)
        .await
        .unwrap();
    let input = page.locator("#i");
    input.focus().await.unwrap();
    expect(input.clone())
        .with_timeout(short())
        .to_be_focused()
        .await
        .expect("to_be_focused");
    expect(input.clone())
        .with_timeout(short())
        .to_have_value_regex(r"\d+@mail\.com")
        .await
        .expect("to_have_value_regex");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn contain_text_regex() {
    let browser = skip_if_none!(common::browser().await);
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<p id="p">Order #9981 confirmed</p>"#)
        .await
        .unwrap();
    let p = page.locator("#p");
    expect(p.clone())
        .with_timeout(short())
        .to_contain_text_regex(r"#\d+")
        .await
        .expect("to_contain_text_regex");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn page_title_and_url_regex() {
    let browser = skip_if_none!(common::browser().await);
    let url = common::http_server(
        "<html><head><title>Dashboard 42</title></head><body>x</body></html>",
    )
    .await;
    let page = browser.new_page().await.unwrap();
    page.goto(&url, None).await.unwrap();
    expect_page(page.clone())
        .with_timeout(short())
        .to_have_title_regex(r"Dashboard \d+")
        .await
        .expect("to_have_title_regex");
    expect_page(page.clone())
        .with_timeout(short())
        .to_have_url_regex(r"127\.0\.0\.1:\d+")
        .await
        .expect("to_have_url_regex");
    browser.close().await.unwrap();
}

/// `to_match_aria_snapshot` does a normalized-equality compare against the
/// locator's `aria_snapshot()`. A self-match (expected == actual snapshot)
/// must pass immediately, proving the method is callable and correctly
/// returns Ok on equality.
#[tokio::test]
async fn aria_snapshot_self_match() {
    let browser = skip_if_none!(common::browser().await);
    let page = browser.new_page().await.unwrap();
    page.set_content(r#"<button>Save</button>"#).await.unwrap();
    let btn = page.locator("button");
    let actual = btn.aria_snapshot().await.expect("aria_snapshot");
    expect(btn.clone())
        .with_timeout(short())
        .to_match_aria_snapshot(&actual)
        .await
        .expect("to_match_aria_snapshot self-match");
    browser.close().await.unwrap();
}
