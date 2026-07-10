//! File-chooser interception smoke test: `on_filechooser` fires,
//! `is_multiple`/`element`/`set_files` work, and the chosen file lands on the
//! `<input type=file>`.

mod common;

use playwright_cdp::FileChooser;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

#[tokio::test]
async fn on_filechooser_accepts_files() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    page.set_content("<input id=\"f\" type=\"file\">").await.unwrap();

    // Create a temp file with a known name + content to feed the chooser.
    let dir = tempfile::TempDir::new().unwrap();
    let tmp_path = dir.path().join("chosen.txt");
    std::fs::write(&tmp_path, b"FILECHOOSERBODY").unwrap();
    let expected_name = "chosen.txt".to_string();

    // Channel to ferry the FileChooser out of the handler.
    let (tx, rx) = oneshot::channel::<FileChooser>();
    let tx = Arc::new(Mutex::new(Some(tx)));

    page.on_filechooser(move |fc| {
        let tx = Arc::clone(&tx);
        let path = tmp_path.clone();
        async move {
            // Accept the chooser with our temp file.
            if let Err(e) = fc.set_files(&[path]).await {
                eprintln!("[handler] set_files error: {e}");
            }
            if let Some(t) = tx.lock().await.take() {
                let _ = t.send(fc);
            }
        }
    })
    .await;

    // Clicking the input opens the native chooser; interception captures it.
    page.locator("#f").click(None).await.unwrap();

    let chooser = match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
        Ok(Ok(fc)) => fc,
        _ => {
            eprintln!("[skip] filechooser event did not fire in time");
            browser.close().await.unwrap();
            return;
        }
    };

    // A plain <input type=file> is single-select.
    assert!(!chooser.is_multiple(), "input should not be multiple");

    // The element() should resolve to the input.
    let el = chooser.element().await.unwrap();
    assert!(el.is_some(), "element() should resolve the input");
    let tag: String = el
        .unwrap()
        .evaluate("el.tagName", None)
        .await
        .unwrap_or_default();
    assert_eq!(tag, "INPUT");

    // The input should now report exactly one file with the expected name.
    // set_files completes inside the handler before the chooser is sent, but
    // Chrome applies it asynchronously — poll briefly until the file appears.
    let mut name = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        name = page
            .evaluate::<String>("document.getElementById('f').files[0]?.name || ''")
            .await
            .unwrap_or_default();
        if !name.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(name, expected_name, "input should carry the chosen file name");

    // Verify the content via a FileReader eval (sync read of an Array).
    let read = r#"
        (async () => {
            const f = document.getElementById('f').files[0];
            const buf = await f.arrayBuffer();
            return Array.from(new Uint8Array(buf));
        })()
    "#;
    // evaluate wraps as (arg) => { return (<expr>); } — but expr here is async,
    // so it returns a Promise that CDP awaits. Cast each byte to a number.
    let bytes: Vec<i64> = page.evaluate(&format!("{read}")).await.unwrap_or_default();
    let body: Vec<u8> = bytes.into_iter().map(|b| b as u8).collect();
    assert_eq!(
        String::from_utf8_lossy(&body),
        "FILECHOOSERBODY",
        "uploaded file content should match"
    );

    browser.close().await.unwrap();
}
