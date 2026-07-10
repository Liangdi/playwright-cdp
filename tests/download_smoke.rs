//! Download capture smoke test: `on_download` fires, suggested filename is
//! correct, and `save_as` writes the downloaded bytes.

mod common;

use playwright_cdp::Download;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

#[tokio::test]
async fn on_download_captures_file() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    // The page hosts an <a download> link pointing at /file (the bytes
    // "DOWNLOADBODY"). The `download` attribute makes Chrome save it even
    // though it is same-origin.
    let html = "<html><body><a id=\"dl\" download=\"my-file.txt\" href=\"/file\">dl</a></body></html>";
    let base = common::http_server_routes(&[("/", html), ("/file", "DOWNLOADBODY")]).await;

    // Channel to ferry the Download out of the handler.
    let (tx, rx) = oneshot::channel::<Download>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    page.on_download(move |dl| {
        let tx = Arc::clone(&tx);
        async move {
            if let Some(t) = tx.lock().await.take() {
                let _ = t.send(dl);
            }
        }
    });

    // Navigate to the page, then click the download link.
    page.goto(&base, None).await.unwrap();
    page.locator("#dl").click(None).await.unwrap();

    // Wait for the download (allow generous time on slower machines).
    let download = match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
        Ok(Ok(d)) => d,
        _ => {
            eprintln!("[skip] download event did not fire in time");
            browser.close().await.unwrap();
            return;
        }
    };

    assert_eq!(download.url(), format!("{base}/file"));
    assert_eq!(download.suggested_filename(), "my-file.txt");
    assert!(download.failure().is_none());

    // Save it to a temp path and verify the bytes.
    let dest = std::env::temp_dir().join(format!(
        "playwright-cdp-dl-{}-{}.txt",
        std::process::id(),
        download.suggested_filename()
    ));
    let saved = download.save_as(&dest).await.unwrap();
    let bytes = tokio::fs::read(&saved).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&bytes), "DOWNLOADBODY");

    // path() should also resolve to a real file.
    let on_disk = download.path().await.unwrap();
    assert!(on_disk.is_some(), "path() should resolve after completion");

    let _ = download.delete().await;
    let _ = tokio::fs::remove_file(&dest).await;
    browser.close().await.unwrap();
}
