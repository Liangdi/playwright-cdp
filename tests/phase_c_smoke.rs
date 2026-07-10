//! Phase C smoke test: history nav (back/forward), frames (iframes), emulate_media.

mod common;

#[tokio::test]
async fn history_and_frames() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();

    let url_a = common::http_server("<html><head><title>A</title></head><body>A</body></html>").await;
    let url_b = common::http_server("<html><head><title>B</title></head><body>B</body></html>").await;

    page.goto(&url_a, None).await.unwrap();
    page.goto(&url_b, None).await.unwrap();
    assert_eq!(page.title().await.unwrap(), "B");

    page.go_back(None).await.unwrap();
    assert_eq!(page.title().await.unwrap(), "A");

    page.go_forward(None).await.unwrap();
    assert_eq!(page.title().await.unwrap(), "B");

    // wait_for_url
    page.goto(&url_a, None).await.unwrap();
    page.wait_for_url(&url_a, None).await.unwrap();

    // emulate_media does not error
    page.emulate_media(None).await.unwrap();

    browser.close().await.unwrap();
}

#[tokio::test]
async fn frames_include_iframe() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    let html = r#"<html><body>
        <iframe id="f" src="data:text/html,<p>inner</p>"></iframe>
    </body></html>"#;
    page.set_content(html).await.unwrap();
    // give the iframe a moment to attach
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let frames = page.frames().await.unwrap();
    assert!(frames.len() >= 2, "expected >=2 frames, got {}", frames.len());

    let main = page.main_frame();
    let children = main.child_frames();
    assert!(!children.is_empty(), "main frame should have child iframes");

    browser.close().await.unwrap();
}
