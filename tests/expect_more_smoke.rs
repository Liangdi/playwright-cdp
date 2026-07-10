//! Smoke tests for the Playwright-style event-wait assertions
//! `expect_console_message` and `expect_popup`.
//!
//! Skips automatically if no browser is available.

mod common;

use std::time::Duration;

/// `expect_console_message` resolves with the console message whose text
/// contains the predicate.
#[tokio::test]
async fn expect_console_message_matches_log() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // Race the wait against the script that logs the message.
    let (msg, log) = tokio::join!(
        page.expect_console_message("hello", Some(Duration::from_secs(5))),
        page.evaluate::<()>("console.log('hello world')"),
    );
    log.expect("evaluate should succeed");
    let msg = msg.expect("expected a matching console message");
    assert!(msg.text().contains("hello"));

    browser.close().await.unwrap();
}

/// A predicate that never matches must time out and return `Error::Timeout`
/// (rather than hanging forever).
#[tokio::test]
async fn expect_console_message_times_out_when_no_match() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // An unrelated log should not match the predicate, so this times out.
    let wait = page
        .expect_console_message("definitely-not-a-real-substring", Some(Duration::from_millis(500)));
    let (res, _) = tokio::join!(wait, page.evaluate::<()>("console.log('unrelated text')"));

    match res {
        Ok(_) => panic!("expected a timeout error, but a message matched"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("Timeout") || msg.contains("timeout"),
                "expected a Timeout error, got: {msg}"
            );
        }
    }

    browser.close().await.unwrap();
}

/// `expect_popup` resolves with the Page opened via `window.open`.
#[tokio::test]
async fn expect_popup_resolves_with_new_page() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // Race the wait against the script that opens the popup.
    let (popup, open) = tokio::join!(
        page.expect_popup(Some(Duration::from_secs(5))),
        page.evaluate::<()>("window.open('about:blank')"),
    );
    open.expect("evaluate should succeed");
    let popup = popup.expect("expected a popup page");

    // The popup is a distinct target from the opener.
    assert_ne!(popup.target_id(), page.target_id());

    // Clean up the popup so it doesn't outlive the browser.
    let _ = popup.close().await;
    browser.close().await.unwrap();
}
