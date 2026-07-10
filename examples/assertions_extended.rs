//! Extended `expect` assertions: CSS, class, focus, regex text/attribute,
//! and screenshot-baseline matching.
//!
//! Builds a single self-contained page (via `page.set_content`) carrying all
//! the elements under test, then exercises the newer assertion methods with a
//! short per-assertion timeout. The screenshot baseline is written under the
//! system temp dir on first run and matched on the second; any `.png` it leaves
//! behind is cleaned up at the end.
//!
//! Run: `cargo run --example assertions_extended`
//! Requires a Chrome/Chromium on the system, or set `PWC_CHROME_PATH`.

use std::time::Duration;

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::types::Viewport;
use playwright_cdp::{expect, Playwright};

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

    // One self-contained page carrying every element the assertions need.
    page.set_content(
        r#"
        <!doctype html>
        <html><head><title>Assertions Extended</title></head>
        <body>
          <!-- inline-styled element for to_have_css / to_have_css_regex -->
          <div id="colored" style="color: red">colored box</div>

          <!-- multi-class element for to_have_class / to_have_class_regex -->
          <div id="btn" class="btn primary">click me</div>

          <!-- a focusable input; we focus() it before asserting to_be_focused -->
          <input id="field" type="text" />

          <!-- text carrying a number for to_have_text_regex -->
          <p id="order">order #42857 confirmed</p>

          <!-- an attribute whose value mixes letters+digits, for
               to_have_attribute_regex -->
          <span id="tagged" data-code="abc123">tagged</span>

          <!-- a stable, fully-specified block for to_have_screenshot -->
          <div id="badge" style="display:inline-block;width:120px;height:40px;
               background:#3366cc;color:#ffffff;line-height:40px;text-align:center;
               font-family:sans-serif;font-size:14px;">STABLE</div>
        </body></html>
        "#,
    )
    .await?;

    // Each assertion pins a short timeout so a hang surfaces quickly rather
    // than holding the process for the default 5s.
    let t = Duration::from_millis(2000);

    // --- to_have_css: exact computed-style match ---
    // Chrome serializes `color: red` as `rgb(255, 0, 0)`.
    expect(page.locator("#colored"))
        .with_timeout(t)
        .to_have_css("color", "rgb(255, 0, 0)")
        .await?;
    println!("to_have_css(#colored, color, rgb(255, 0, 0))  OK");

    // --- to_have_css_regex: regex match on the same value ---
    expect(page.locator("#colored"))
        .with_timeout(t)
        .to_have_css_regex("color", r"^rgb\(255,\s*0,\s*0\)$")
        .await?;
    println!("to_have_css_regex(#colored, color, ^rgb(255, 0, 0)$)  OK");

    // --- to_have_class: exact class string ---
    expect(page.locator("#btn"))
        .with_timeout(t)
        .to_have_class("btn primary")
        .await?;
    println!("to_have_class(#btn, \"btn primary\")  OK");

    // --- to_have_class_regex: match one class token with a word boundary ---
    expect(page.locator("#btn"))
        .with_timeout(t)
        .to_have_class_regex(r"\bprimary\b")
        .await?;
    println!("to_have_class_regex(#btn, \\bprimary\\b)  OK");

    // --- to_be_focused: focus the input first, then assert ---
    page.locator("#field").focus().await?;
    expect(page.locator("#field"))
        .with_timeout(t)
        .to_be_focused()
        .await?;
    println!("to_be_focused(#field) after focus()  OK");

    // --- to_have_text_regex: text contains digits ---
    expect(page.locator("#order"))
        .with_timeout(t)
        .to_have_text_regex(r"#\d+")
        .await?;
    println!("to_have_text_regex(#order, #\\d+)  OK");

    // --- to_have_attribute_regex: attribute value contains digits ---
    expect(page.locator("#tagged"))
        .with_timeout(t)
        .to_have_attribute_regex("data-code", r"\d+")
        .await?;
    println!("to_have_attribute_regex(#tagged, data-code, \\d+)  OK");

    // --- to_have_screenshot: baseline on first run, match on second ---
    // The baseline lives in the system temp dir so we don't litter the repo.
    // On the very first run there is no baseline, so the call writes it and
    // succeeds; on a second run it byte-compares against it.
    let mut baseline = std::env::temp_dir();
    baseline.push("pwc-assertions-extended-badge");
    let baseline_path = baseline.to_string_lossy().into_owned();
    println!("screenshot baseline path: {baseline_path}.png");

    expect(page.locator("#badge"))
        .with_timeout(t)
        .to_have_screenshot(&baseline_path, None)
        .await?;
    println!("to_have_screenshot(#badge) first pass (baseline written)  OK");

    // Second pass: now a baseline exists, so this actually compares pixels.
    expect(page.locator("#badge"))
        .with_timeout(t)
        .to_have_screenshot(&baseline_path, None)
        .await?;
    println!("to_have_screenshot(#badge) second pass (matched baseline)  OK");

    // Clean up the baseline (and any -actual.png dump) we generated.
    let _ = std::fs::remove_file(format!("{baseline_path}.png"));
    let _ = std::fs::remove_file(format!("{baseline_path}-actual.png"));

    println!("all extended assertions passed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
