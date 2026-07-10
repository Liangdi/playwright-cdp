//! `Coverage` API demo: collect precise JS coverage for a page.
//!
//! Order matters — `Profiler.startPreciseCoverage` must be invoked *before*
//! the script is compiled/executed, otherwise the script never gets reported
//! as covered. So we: start coverage -> inject HTML+script -> run the script
//! via `evaluate` -> stop coverage and inspect the entries.
//!
//! Run: `cargo run --example coverage_example`

use playwright_cdp::coverage::JSCoverageStartOptions;
use playwright_cdp::options::LaunchOptions;
use playwright_cdp::Playwright;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "playwright_cdp=info".into()),
        )
        .try_init();

    // --- launch + new page ---
    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;

    // 1) Start JS coverage BEFORE the script is compiled/run. Detailed
    //    (per-block) coverage is on by default; we pass options explicitly to
    //    show the shape of the API.
    page.coverage()
        .start_js_coverage(Some(
            JSCoverageStartOptions::default()
                .report_anonymous(true)
                .include_block_coverage(true),
        ))
        .await?;
    println!("js coverage started");

    // 2) Inject the page content + script, then drive execution.
    page.set_content(
        r#"<html><body><script>function used(){return 1;} function unused(){return 2;} window.__r = used();</script></body></html>"#,
    )
    .await?;
    // Force the script to run (set_content already executes inline scripts,
    // but this also pins `window.__r` as a typed result we can assert on).
    let r: i64 = page.evaluate("window.__r").await?;
    println!("window.__r = {r}");
    assert_eq!(r, 1, "the script should have run and set window.__r = 1");

    // 3) Stop coverage and read the report.
    let res = page.coverage().stop_js_coverage().await?;
    let entry_count = res.entries.len();
    println!("js coverage entries: {entry_count}");
    assert!(
        entry_count >= 1,
        "at least one script should be reported as covered, got {entry_count}"
    );

    // 4) Print each entry's url + function/range counts.
    for (i, entry) in res.entries.iter().enumerate() {
        let url = if entry.url.is_empty() {
            "<inline>"
        } else {
            &entry.url
        };
        let total_ranges: usize = entry.functions.iter().map(|f| f.ranges.len()).sum();
        println!(
            "  [{i}] script_id={} url={} functions={} ranges={}",
            entry.script_id,
            url,
            entry.functions.len(),
            total_ranges,
        );
        for func in &entry.functions {
            let name = if func.function_name.is_empty() {
                "<anonymous>"
            } else {
                &func.function_name
            };
            let executed: i64 = func
                .ranges
                .iter()
                .map(|r| if r.call_count > 0 { 1 } else { 0 })
                .sum();
            println!(
                "      fn `{name}` block_coverage={} ranges={} executed_ranges={}",
                func.is_block_coverage,
                func.ranges.len(),
                executed,
            );
        }
    }

    browser.close().await?;
    println!("PASS");
    Ok(())
}
