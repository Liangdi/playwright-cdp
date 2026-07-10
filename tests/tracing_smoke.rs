//! Performance `Tracing` smoke tests (CDP `Tracing` domain).

mod common;

use playwright_cdp::options::TracingStartOptions;

#[tokio::test]
async fn tracing_start_stop_returns_trace() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let context = browser.new_context().await.unwrap();
    let page = context.new_page().await.unwrap();

    // Start tracing with an explicit category BEFORE navigating so the trace
    // captures real activity.
    context
        .tracing()
        .start(Some(
            TracingStartOptions::new().categories(vec!["devtools.timeline".to_string()]),
        ))
        .await
        .unwrap();

    // Generate some activity: a couple of navigations.
    page.set_content("<html><body><h1>tracing test</h1></body></html>")
        .await
        .unwrap();
    let base = common::http_server("<html><body><p>hello trace</p></body></html>").await;
    page.goto(&base, None).await.unwrap();
    let _title: String = page.evaluate("document.title").await.unwrap();

    let bytes = context.tracing().stop().await.unwrap();

    // The trace should be a non-trivial JSON document.
    assert!(
        bytes.len() > 1024,
        "trace too small: {} bytes",
        bytes.len()
    );

    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("traceEvents"),
        "trace JSON should contain 'traceEvents', got: {}",
        &text[..text.len().min(200)]
    );

    browser.close().await.unwrap();
}

#[tokio::test]
async fn tracing_writes_to_path() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let context = browser.new_context().await.unwrap();
    let page = context.new_page().await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let trace_path = dir.path().join("trace.json");
    let trace_path_str = trace_path.to_string_lossy().to_string();

    context
        .tracing()
        .start(Some(
            TracingStartOptions::new()
                .categories(vec!["devtools.timeline".to_string()])
                .path(&trace_path_str),
        ))
        .await
        .unwrap();

    page.set_content("<html><body><h1>path test</h1></body></html>")
        .await
        .unwrap();

    let bytes = context.tracing().stop().await.unwrap();

    // File should exist and be non-empty, matching the returned bytes.
    let meta = tokio::fs::metadata(&trace_path).await.unwrap();
    assert!(meta.len() > 1024, "trace file too small: {} bytes", meta.len());
    assert_eq!(meta.len() as usize, bytes.len());

    let on_disk = tokio::fs::read(&trace_path).await.unwrap();
    let text = String::from_utf8_lossy(&on_disk);
    assert!(text.contains("traceEvents"), "file trace missing traceEvents");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn tracing_stop_without_start_errors() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let context = browser.new_context().await.unwrap();
    let res = context.tracing().stop().await;
    assert!(res.is_err(), "stop without start should error");
    browser.close().await.unwrap();
}
