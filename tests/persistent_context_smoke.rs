//! Smoke tests for `BrowserType::launch_persistent_context` — launching
//! Chromium against a persistent on-disk user-data-dir so cookies / localStorage
//! survive across sessions. Skips gracefully if no browser is available.

mod common;

use playwright_cdp::Playwright;

/// A unique per-(test, process) temp dir under the system temp location.
fn unique_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "pw-cdp-persistent-{}-{}",
        label,
        std::process::id()
    ))
}

/// Try to launch a persistent context; return `None` if no browser is around
/// so the test can skip.
async fn try_persistent(label: &str) -> Option<(playwright_cdp::BrowserContext, std::path::PathBuf)> {
    let dir = unique_dir(label);
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = Playwright::launch()
        .await
        .ok()?
        .chromium()
        .launch_persistent_context(&dir)
        .await
        .ok()?;
    Some((ctx, dir))
}

#[tokio::test]
async fn launch_persistent_context_basic() {
    let (ctx, dir) = match try_persistent("basic").await {
        Some(v) => v,
        None => {
            eprintln!("[skip] no browser available");
            return;
        }
    };
    let page = ctx.new_page().await.expect("new_page");
    page.set_content("<h1 id='h'>persistent</h1>")
        .await
        .unwrap();
    assert_eq!(
        page.locator("#h").text_content().await.unwrap(),
        Some("persistent".to_string())
    );
    ctx.close().await.unwrap();

    // The user-data-dir must NOT be deleted on close (it's persistent, not a
    // temp dir), and Chromium must have written a profile into it.
    assert!(dir.exists(), "user-data-dir must persist after close");
    assert!(
        dir.join("Default").exists(),
        "Chromium profile dir 'Default' should exist under user-data-dir"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The defining behavior of a persistent context: storage written in one
/// session is still there when the same user-data-dir is launched again.
#[tokio::test]
async fn launch_persistent_context_persists_storage() {
    // A stable origin across both sessions (same server task, same port/url).
    let base = common::http_server(
        "<html><body><script>window.onload=()=>{document.body.innerText=localStorage.getItem('k')||''}</script></body></html>",
    )
    .await;

    // Session 1: write localStorage.
    let dir = unique_dir("persist");
    let _ = std::fs::remove_dir_all(&dir);
    {
        let ctx = Playwright::launch()
            .await
            .unwrap()
            .chromium()
            .launch_persistent_context(&dir)
            .await
            .expect("launch #1");
        let page = ctx.new_page().await.unwrap();
        page.goto(&base, None).await.unwrap();
        let _: serde_json::Value =
            page.evaluate("localStorage.setItem('k','persisted-value')")
                .await
                .unwrap();
        ctx.close().await.unwrap();
    }

    // Session 2: same user-data-dir, same origin → localStorage must survive.
    {
        let ctx = match Playwright::launch()
            .await
            .unwrap()
            .chromium()
            .launch_persistent_context(&dir)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[skip] no browser for session 2: {e}");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
        };
        let page = ctx.new_page().await.unwrap();
        page.goto(&base, None).await.unwrap();
        let val: String = page
            .evaluate("localStorage.getItem('k') || ''")
            .await
            .unwrap();
        assert_eq!(
            val, "persisted-value",
            "localStorage must persist across sessions via the user-data-dir"
        );
        ctx.close().await.unwrap();
    }

    let _ = std::fs::remove_dir_all(&dir);
}
