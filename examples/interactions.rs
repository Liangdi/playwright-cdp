//! Element interactions + input devices (keyboard / mouse / touchscreen).
//!
//! Builds a self-contained form-like page via `set_content` (no network) and
//! drives it through locators, then the raw `page.keyboard()` / `page.mouse()` /
//! `page.touchscreen()` input devices.
//!
//! Run: `cargo run --example interactions`

use playwright_cdp::options::{LaunchOptions, SelectOption};
use playwright_cdp::types::Viewport;
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;
    page.set_viewport_size(Viewport::new(800, 600)).await?;

    // A single self-contained page: a form, a click-toggle button, a dblclick
    // counter, a hover target, and a touch target.
    page.set_content(
        r#"
        <!doctype html>
        <html><head><title>Interactions</title></head>
        <body>
          <input id="name" type="text" placeholder="your name">
          <input id="agree" type="checkbox">
          <select id="color">
            <option value="red">red</option>
            <option value="green">green</option>
            <option value="blue">blue</option>
          </select>
          <button id="go">Go</button>

          <div id="dbl" data-count="0">double-click me</div>
          <div id="hover">hover me</div>
          <div id="touch" data-taps="0">tap me</div>

          <script>
            document.getElementById('go').addEventListener('click', () => {
              window.__clicked = true;
            });
            const dbl = document.getElementById('dbl');
            dbl.addEventListener('dblclick', () => {
              let n = Number(dbl.dataset.count) + 1;
              dbl.dataset.count = String(n);
              dbl.textContent = 'double-clicked x' + n;
            });
            const hover = document.getElementById('hover');
            hover.addEventListener('mouseenter', () => {
              hover.textContent = 'hovered!';
            });
            const touch = document.getElementById('touch');
            touch.addEventListener('touchstart', () => {
              let n = Number(touch.dataset.taps) + 1;
              touch.dataset.taps = String(n);
              touch.textContent = 'tapped x' + n;
            });
          </script>
        </body></html>
        "#,
    )
    .await?;

    // --- fill + read back ---
    page.locator("#name").fill("alice", None).await?;
    let name_value = page.locator("#name").input_value(None).await?;
    println!("name input after fill: {name_value}");
    assert_eq!(name_value, "alice");

    // --- checkbox ---
    page.locator("#agree").check(None).await?;
    let checked = page.locator("#agree").is_checked().await?;
    println!("agree checked: {checked}");
    assert!(checked);

    // --- select option by value, read selected ---
    let selected = page
        .locator("#color")
        .select_option(SelectOption::value("blue"), None)
        .await?;
    println!("select_option returned: {selected:?}");
    let color_value = page.locator("#color").input_value(None).await?;
    println!("color value after select: {color_value}");
    assert_eq!(color_value, "blue");

    // --- click toggling a global, then verify ---
    page.locator("#go").click(None).await?;
    let clicked: bool = page.evaluate("window.__clicked === true").await?;
    println!("go button set window.__clicked: {clicked}");
    assert!(clicked);

    // --- double-click counter ---
    page.locator("#dbl").dblclick(None).await?;
    let dbl_count: String = page
        .evaluate("document.getElementById('dbl').dataset.count")
        .await?;
    println!("dblclick count: {dbl_count}");
    assert_eq!(dbl_count, "1");

    // --- hover changes text ---
    page.locator("#hover").hover(None).await?;
    let hover_text: String = page
        .evaluate("document.getElementById('hover').textContent")
        .await?;
    println!("hover text after hover: {hover_text}");
    assert_eq!(hover_text, "hovered!");

    // --- press Enter on the name field, then drive the raw keyboard ---
    // Focus first, then press; press focuses again but be explicit for clarity.
    page.locator("#name").focus().await?;
    page.locator("#name").press("Enter", None).await?;
    // Append more text via page.keyboard().type_ on the focused input.
    page.keyboard().type_(" bob", None).await?;
    let after_type: String = page.locator("#name").input_value(None).await?;
    println!("name after keyboard.type_: {after_type}");
    assert_eq!(after_type, "alice bob");

    // --- raw mouse: move over the hover target and click the Go button ---
    // Use the element box so we click inside it without hardcoding pixels.
    let go_box = page.locator("#go").bounding_box().await?.unwrap();
    let go_cx = go_box.x + go_box.width / 2.0;
    let go_cy = go_box.y + go_box.height / 2.0;
    page.mouse().move_to(go_cx, go_cy, None).await?;
    page.mouse()
        .click(go_cx, go_cy, None)
        .await?;
    let click_count: i64 = page
        .evaluate("window.__clicked ? 1 : 0")
        .await?;
    println!("mouse click landed on go button (window.__clicked truthy): {click_count}");
    assert_eq!(click_count, 1);

    // --- touchscreen: tap the touch target ---
    let touch_box = page.locator("#touch").bounding_box().await?.unwrap();
    let tx = touch_box.x + touch_box.width / 2.0;
    let ty = touch_box.y + touch_box.height / 2.0;
    page.touchscreen().tap(tx, ty).await?;
    let taps: String = page
        .evaluate("document.getElementById('touch').dataset.taps")
        .await?;
    println!("touch taps after touchscreen.tap: {taps}");
    assert_eq!(taps, "1");

    println!("all interactions succeeded.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
