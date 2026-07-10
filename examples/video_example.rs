//! `Video` — screencast frame capture.
//!
//! Demonstrates `page.video()` (built on CDP `Page.startScreencast`):
//! start a cast, let an animated page produce frames, stop, and verify
//! `frame_count() > 0`.
//!
//! NOTE: the output is a sequence of captured PNG/JPEG frames held in
//! memory (and optionally persisted to `path`), **not** a muxed video file.
//! That is expected; this example only asserts frames were captured.
//!
//! Run: `cargo run --example video_example`

use std::time::Duration;

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::types::Viewport;
use playwright_cdp::video::VideoStartOptions;
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    // Launch headless Chromium.
    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    page.set_viewport_size(Viewport::new(800, 600)).await?;

    // 1. Acquire the video handle before recording starts.
    let video = page.video();
    assert!(
        !video.is_recording(),
        "video should not be recording before start()"
    );
    println!("video.is_recording() before start: {}", video.is_recording());

    // 2. Start a screencast capped to the viewport. Frames accumulate in memory.
    video
        .start(Some(
            VideoStartOptions::default()
                .max_width(800)
                .max_height(600),
        ))
        .await?;
    assert!(
        video.is_recording(),
        "video should be recording after start()"
    );
    println!("video.is_recording() after start: {}", video.is_recording());

    // 3. Inject animated content so the page keeps repainting and producing
    //    screencast frames (a 100ms interval flipping a div's background).
    page.set_content(
        r#"<html><body><div id="a" style="width:100px;height:100px;background:red"></div><script>let i=0;const d=document.getElementById('a');setInterval(()=>{d.style.background=(i++%2?'blue':'red');},100);</script></body></html>"#,
    )
    .await?;

    // 4. Give the cast time to capture several frames.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Stop the cast.
    video.stop().await?;
    assert!(
        !video.is_recording(),
        "video should not be recording after stop()"
    );
    println!("video.is_recording() after stop: {}", video.is_recording());

    // 6. Verify at least one frame was captured (in-memory count; no file).
    let frames = video.frame_count();
    println!("video.frame_count(): {frames}");
    assert!(
        frames > 0,
        "screencast should have captured at least one frame"
    );

    if let Some(path) = video.path() {
        println!("video.path(): {}", path.display());
    } else {
        println!("video.path(): None (in-memory capture, nothing persisted)");
    }

    browser.close().await?;
    println!("PASS");
    Ok(())
}
