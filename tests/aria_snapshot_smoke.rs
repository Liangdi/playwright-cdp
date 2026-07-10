//! Smoke test for `Page::aria_snapshot` / `Locator::aria_snapshot`. Skips if
//! no browser is available.

mod common;

#[tokio::test]
async fn page_aria_snapshot() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(
        r#"<main><h1>Welcome</h1><button id="b">Submit</button><ul><li>First</li><li>Second</li></ul><input type="text" aria-label="Email"></main>"#,
    )
    .await
    .unwrap();

    let snap = page.aria_snapshot().await.unwrap();
    println!("=== page aria snapshot ===\n{snap}=== end ===");

    // Heading + Welcome.
    assert!(
        snap.contains("heading") && snap.contains("Welcome"),
        "expected heading/Welcome in:\n{snap}"
    );
    // Button + Submit.
    assert!(
        snap.contains("button") && snap.contains("Submit"),
        "expected button/Submit in:\n{snap}"
    );
    // list + listitem + items.
    assert!(snap.contains("list"), "expected list in:\n{snap}");
    assert!(
        snap.contains("listitem") && snap.contains("First") && snap.contains("Second"),
        "expected listitem First/Second in:\n{snap}"
    );

    browser.close().await.unwrap();
}

#[tokio::test]
async fn locator_aria_snapshot_is_scoped() {
    let browser = match common::browser().await {
        Some(b) => b,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = browser.new_page().await.unwrap();
    page.set_content(
        r#"<main><h1>Welcome</h1><button id="b">Submit</button><ul><li>First</li><li>Second</li></ul><input type="text" aria-label="Email"></main>"#,
    )
    .await
    .unwrap();

    let loc_snap = page.locator("ul").aria_snapshot().await.unwrap();
    println!("=== locator (ul) aria snapshot ===\n{loc_snap}=== end ===");

    assert!(
        loc_snap.contains("listitem") && loc_snap.contains("First") && loc_snap.contains("Second"),
        "expected listitem First/Second in:\n{loc_snap}"
    );
    // Scoped to the ul: the heading text must not leak in.
    assert!(
        !loc_snap.contains("Welcome"),
        "scoped snapshot should not contain Welcome, got:\n{loc_snap}"
    );

    browser.close().await.unwrap();
}
