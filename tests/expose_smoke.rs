//! `Page::expose_function` smoke tests.

mod common;

use serde_json::json;

#[tokio::test]
async fn expose_function_returns_value_from_rust() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.expose_function("add", |args| async move {
        let a = args[0].as_i64().unwrap();
        let b = args[1].as_i64().unwrap();
        json!(a + b)
    })
    .await
    .unwrap();

    // Rewrite the document so the wrapper is fresh in the active context.
    page.set_content("<html><body></body></html>")
        .await
        .unwrap();

    // `add` returns a Promise; page.evaluate awaits it (awaitPromise: true).
    let r: i64 = page.evaluate("add(2, 3)").await.unwrap();
    assert_eq!(r, 5);

    browser.close().await.unwrap();
}

#[tokio::test]
async fn expose_function_passes_args_and_returns_json() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.expose_function("echo", |args| async move {
        json!({ "count": args.len(), "first": args.get(0).cloned().unwrap_or(serde_json::Value::Null) })
    })
    .await
    .unwrap();

    page.set_content("<html><body></body></html>")
        .await
        .unwrap();

    let out: serde_json::Value = page
        .evaluate("echo('hello', true, 42)")
        .await
        .unwrap();
    assert_eq!(out["count"], 3);
    assert_eq!(out["first"], "hello");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn expose_function_works_across_navigation() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.expose_function("upper", |args| async move {
        let s = args[0].as_str().unwrap_or("").to_string();
        json!(s.to_uppercase())
    })
    .await
    .unwrap();

    // Real navigation to a new document; the init-script wrapper is re-run and
    // the CDP binding persists on the page session.
    page.goto(
        "data:text/html,<html><body><p>nav</p></body></html>",
        None,
    )
    .await
    .unwrap();

    let r: String = page.evaluate("upper('abc')").await.unwrap();
    assert_eq!(r, "ABC");

    browser.close().await.unwrap();
}
