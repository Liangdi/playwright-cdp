//! Playwright-style `expect` / `expect_page` assertions.
//!
//! Builds a self-contained page with a title, a timer-driven counter, a toggle,
//! a hidden element, and an element whose text flips after a short delay — then
//! asserts over them with the auto-retry assertion API.
//!
//! Run: `cargo run --example assertions`

use std::time::Duration;

use playwright_cdp::options::LaunchOptions;
use playwright_cdp::types::Viewport;
use playwright_cdp::{expect, expect_page, Playwright};

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

    page.set_content(
        r#"
        <!doctype html>
        <html><head><title>Assertions Demo</title></head>
        <body>
          <h1 id="heading">hello</h1>

          <!-- a counter that increments on a timer -->
          <div id="counter">0</div>

          <!-- a toggle button that flips a sibling's visibility -->
          <button id="toggle">toggle</button>
          <div id="panel">visible panel</div>

          <!-- an element that is hidden from the start -->
          <div id="secret" style="display:none">hidden</div>

          <!-- three items, for to_have_count -->
          <ul><li class="item">a</li><li class="item">b</li><li class="item">c</li></ul>

          <!-- a link with a fixed attribute -->
          <a id="link" href="https://example.com" data-role="primary">link</a>

          <!-- an element whose text flips after ~200ms -->
          <div id="async">pending</div>

          <script>
            let n = 0;
            setInterval(() => {
              n += 1;
              document.getElementById('counter').textContent = String(n);
            }, 100);

            const toggle = document.getElementById('toggle');
            const panel = document.getElementById('panel');
            let shown = true;
            toggle.addEventListener('click', () => {
              shown = !shown;
              panel.style.display = shown ? '' : 'none';
            });

            setTimeout(() => {
              document.getElementById('async').textContent = 'done';
            }, 200);
          </script>
        </body></html>
        "#,
    )
    .await?;

    // --- page-level assertions ---
    expect_page(page.clone()).to_have_title("Assertions Demo").await?;
    println!("to_have_title: Assertions Demo  OK");

    // set_content keeps the about:blank URL; assert against it.
    let current_url = page.url().await?;
    println!("page url after set_content: {current_url}");
    expect_page(page.clone()).to_have_url(&current_url).await?;
    println!("to_have_url: {current_url}  OK");

    // --- locator visibility + text ---
    expect(page.locator("#heading")).to_be_visible().await?;
    println!("to_be_visible(#heading)  OK");
    expect(page.locator("#heading")).to_have_text("hello").await?;
    println!("to_have_text(#heading, hello)  OK");

    // --- negated visibility on a hidden element ---
    expect(page.locator("#secret")).not().to_be_visible().await?;
    println!("not().to_be_visible(#secret)  OK");

    // --- count + attribute + contains ---
    expect(page.locator(".item")).to_have_count(3).await?;
    println!("to_have_count(.item, 3)  OK");
    expect(page.locator("#link"))
        .to_have_attribute("data-role", "primary")
        .await?;
    println!("to_have_attribute(#link, data-role, primary)  OK");
    expect(page.locator("#heading"))
        .to_contain_text("ell")
        .await?;
    println!("to_contain_text(#heading, ell)  OK");

    // --- async-ish: the #async element flips to "done" after ~200ms; give the
    //     assertion a generous timeout so it passes once the timer fires. ---
    expect(page.locator("#async"))
        .with_timeout(Duration::from_secs(3))
        .to_have_text("done")
        .await?;
    let async_text: String = page
        .evaluate("document.getElementById('async').textContent")
        .await?;
    println!("async element settled to: {async_text}");
    assert_eq!(async_text, "done");

    // --- toggle hides the panel; the negated assertion retries until it holds ---
    page.locator("#toggle").click(None).await?;
    expect(page.locator("#panel")).not().to_be_visible().await?;
    println!("after toggle click: not().to_be_visible(#panel)  OK");

    println!("all assertions passed.");
    browser.close().await?;
    println!("done.");
    Ok(())
}
