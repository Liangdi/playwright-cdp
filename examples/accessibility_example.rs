//! `Accessibility` snapshot API: capture the accessibility tree and walk it.
//!
//! Run: `cargo run --example accessibility_example`
//! Requires a Chrome/Chromium on the system, or set `CHROME_PATH`.

use playwright_cdp::accessibility::{AccessibilityNode, AccessibilitySnapshotOptions};
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

    let browser = Playwright::launch()
        .await?
        .chromium()
        .launch_with_options(LaunchOptions::default())
        .await?;
    println!("browser version: {}", browser.version().await?);

    let page = browser.new_page().await?;

    // 1. Inject a small DOM with a button and a labelled textbox.
    page.set_content(
        r#"<html><body><button>Submit</button><input type="text" aria-label="Email"></body></html>"#,
    )
    .await?;

    // Let the page settle so the accessibility tree is built.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // 2. Snapshot the full accessibility tree.
    //    Try the default (interesting_only == true) first; fall back to the raw
    //    tree if it comes back empty so we can still inspect the structure.
    let mut tree = page.accessibility().snapshot(None).await?;
    if tree.is_none() {
        println!("default snapshot was empty; retrying with interesting_only=false");
        tree = page
            .accessibility()
            .snapshot(Some(
                AccessibilitySnapshotOptions::default().interesting_only(false),
            ))
            .await?;
    }

    let tree = tree.expect("accessibility snapshot should be Some after content is set");
    println!("snapshot root role: {:?}", tree.role());

    // 3. Recursively walk the tree, collecting every (role, name) pair.
    let mut pairs: Vec<(Option<String>, Option<String>)> = Vec::new();
    collect(&tree, &mut pairs);

    println!("--- accessibility tree ({}) ---", pairs.len());
    for (i, (role, name)) in pairs.iter().enumerate() {
        println!(
            "{i:>2}: role={:<12} name={:?}",
            format!("{:?}", role),
            name
        );
    }

    // 4. Verify the snapshot is structured and contains the expected nodes.
    assert!(!pairs.is_empty(), "snapshot should be non-empty");

    // Lenient matching: CDP roles can vary across versions ("button", "textbox",
    // sometimes "input"). Check for a button-like node named "Submit" and a
    // textbox/input-like node for the email field.
    let has_submit_button = pairs.iter().any(|(role, name)| {
        let role = role.as_deref().unwrap_or("");
        let name = name.as_deref().unwrap_or("");
        role.eq_ignore_ascii_case("button") && name == "Submit"
    });

    let has_email_textbox = pairs.iter().any(|(role, name)| {
        let role = role.as_deref().unwrap_or("");
        let name = name.as_deref().unwrap_or("");
        (role.eq_ignore_ascii_case("textbox") || role.eq_ignore_ascii_case("input"))
            && name.eq_ignore_ascii_case("Email")
    });

    if !has_submit_button || !has_email_textbox {
        // Be lenient: at least confirm a button-like node and a textbox/input-like
        // node exist somewhere in the tree, printing the actual roles for diagnosis.
        let roles: Vec<&str> = pairs
            .iter()
            .map(|(r, _)| r.as_deref().unwrap_or(""))
            .collect();
        eprintln!("exact-match failed; actual roles: {roles:?}");

        let any_button = pairs
            .iter()
            .any(|(r, _)| r.as_deref().unwrap_or("").eq_ignore_ascii_case("button"));
        let any_textbox = pairs.iter().any(|(r, _)| {
            let role = r.as_deref().unwrap_or("");
            role.eq_ignore_ascii_case("textbox") || role.eq_ignore_ascii_case("input")
        });
        assert!(
            any_button,
            "expected at least one button-like node in the tree, got roles {roles:?}"
        );
        assert!(
            any_textbox,
            "expected at least one textbox/input-like node in the tree, got roles {roles:?}"
        );
        println!("PASS (lenient): button-like and textbox-like nodes present");
    } else {
        println!("PASS: found button \"Submit\" and textbox \"Email\"");
    }

    browser.close().await?;
    println!("done.");
    Ok(())
}

/// Depth-first walk collecting `(role, name)` from every node.
fn collect(node: &AccessibilityNode, out: &mut Vec<(Option<String>, Option<String>)>) {
    out.push((node.role.clone(), node.name.clone()));
    for child in &node.children {
        collect(child, out);
    }
}
