//! Playwright-style `expect(locator)` / `expect(page)` assertions.
//!
//! Each assertion polls its underlying [`Locator`] / [`Page`] method every
//! ~100ms until the condition holds, or the assertion timeout (default 5s)
//! elapses, in which case [`Error::Timeout`] is returned with a message naming
//! the assertion and the actual-vs-expected values.
//!
//! Negation via [`LocatorAssertions::not`] / [`PageAssertions::not`] flips the
//! sense: a negated assertion succeeds when the base condition is **false**
//! within the timeout.

use std::future::Future;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::locator::Locator;
use crate::page::Page;

/// Default assertion timeout (mirrors Playwright's `expect` default).
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);
/// Polling interval between condition checks.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

// --- top-level constructors ---

/// Construct [`LocatorAssertions`] for `locator`, mirroring Playwright's
/// `expect(locator)`.
pub fn expect(locator: Locator) -> LocatorAssertions {
    LocatorAssertions::new(locator)
}

/// Construct [`PageAssertions`] for `page`. Mirrors Playwright's
/// `expect(page)`. (Distinct name from [`expect`] because Rust cannot overload
/// `expect` on the parameter type without a trait.)
pub fn expect_page(page: Page) -> PageAssertions {
    PageAssertions::new(page)
}

/// Poll `cond` every [`POLL_INTERVAL`] until it returns `Ok(want)`, where
/// `want = !negated`. Returns `Ok(())` on success, or `Err(Error::Timeout(msg))`
/// on timeout. The `msg` closure produces the failure message lazily.
///
/// `cond` errors are propagated immediately (they are not assertion failures).
async fn wait_for<F, Fut, Msg>(cond: F, timeout: Duration, negated: bool, msg: Msg) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<bool>>,
    Msg: FnOnce() -> String,
{
    let want = !negated;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let got = cond().await?;
        if got == want {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Timeout(msg()));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ===========================================================================
// LocatorAssertions
// ===========================================================================

/// Assertions over a [`Locator`], produced by [`expect`].
#[derive(Clone)]
pub struct LocatorAssertions {
    locator: Locator,
    timeout: Duration,
    negated: bool,
}

impl LocatorAssertions {
    /// Wrap a locator with the default 5s timeout, non-negated.
    pub fn new(locator: Locator) -> Self {
        Self {
            locator,
            timeout: DEFAULT_TIMEOUT,
            negated: false,
        }
    }

    /// Builder: override the assertion timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the assertion timeout in place.
    pub fn set_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    /// Return a negated clone: succeeding when the base condition is false.
    pub fn not(&self) -> Self {
        let mut c = self.clone();
        c.negated = true;
        c
    }

    fn sel(&self) -> &str {
        self.locator.selector()
    }

    // --- visibility / state ---

    pub async fn to_be_visible(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_visible().await }
            },
            timeout,
            negated,
            || format!("to_be_visible('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    pub async fn to_be_hidden(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_hidden().await }
            },
            timeout,
            negated,
            || format!("to_be_hidden('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    pub async fn to_be_enabled(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_enabled().await }
            },
            timeout,
            negated,
            || format!("to_be_enabled('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    pub async fn to_be_disabled(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_disabled().await }
            },
            timeout,
            negated,
            || format!("to_be_disabled('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    pub async fn to_be_editable(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_editable().await }
            },
            timeout,
            negated,
            || format!("to_be_editable('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    pub async fn to_be_checked(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_checked().await }
            },
            timeout,
            negated,
            || format!("to_be_checked('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    pub async fn to_be_unchecked(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move {
                    // unchecked == !checked
                    Ok(!l.is_checked().await?)
                }
            },
            timeout,
            negated,
            || format!("to_be_unchecked('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    // --- text ---

    /// Assert the element's trimmed `textContent` equals `expected`.
    pub async fn to_have_text(&self, expected: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected_owned.clone();
        wait_for(
            move || {
                let l = l.clone();
                let expected_owned = expected_owned.clone();
                async move {
                    let actual = l.text_content().await?;
                    let actual = actual.unwrap_or_default();
                    Ok(actual.trim() == expected_owned.trim())
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_text('{sel}', '{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's trimmed `textContent` contains `expected`.
    pub async fn to_contain_text(&self, expected: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected_owned.clone();
        wait_for(
            move || {
                let l = l.clone();
                let expected_owned = expected_owned.clone();
                async move {
                    let actual = l.text_content().await?;
                    Ok(actual
                        .unwrap_or_default()
                        .trim()
                        .contains(expected_owned.trim()))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_contain_text('{sel}', '{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's `innerText` equals `expected`.
    pub async fn to_have_inner_text(&self, expected: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected_owned.clone();
        wait_for(
            move || {
                let l = l.clone();
                let expected_owned = expected_owned.clone();
                async move {
                    let actual = l.inner_text().await?;
                    Ok(actual == expected_owned)
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_inner_text('{sel}', '{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    // --- count ---

    /// Assert the number of matching elements equals `expected`.
    pub async fn to_have_count(&self, expected: usize) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { Ok(l.count().await? == expected) }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_count('{sel}', {expected}) timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    // --- attributes / value ---

    /// Assert the element has attribute `name` equal to `value`.
    pub async fn to_have_attribute(&self, name: &str, value: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let name_owned = name.to_string();
        let value_owned = value.to_string();
        let name_for_msg = name.to_string();
        let value_for_msg = value.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let name_owned = name_owned.clone();
                let value_owned = value_owned.clone();
                async move {
                    let attr = l.get_attribute(&name_owned).await?;
                    Ok(attr.as_deref() == Some(value_owned.as_str()))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_attribute('{sel}', '{name_for_msg}', '{value_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's input value equals `expected`.
    pub async fn to_have_value(&self, expected: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let expected_owned = expected_owned.clone();
                async move { Ok(l.input_value(None).await? == expected_owned) }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_value('{sel}', '{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element is empty: an empty input value, or zero matching
    /// elements.
    pub async fn to_be_empty(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move {
                    let count = l.count().await?;
                    if count == 0 {
                        return Ok(true);
                    }
                    // For a present element, "empty" means an empty input value.
                    let value = l.input_value(None).await.unwrap_or_default();
                    Ok(value.is_empty())
                }
            },
            timeout,
            negated,
            move || format!("to_be_empty('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }
}

// ===========================================================================
// PageAssertions
// ===========================================================================

/// Assertions over a [`Page`], produced by [`expect_page`].
#[derive(Clone)]
pub struct PageAssertions {
    page: Page,
    timeout: Duration,
    negated: bool,
}

impl PageAssertions {
    /// Wrap a page with the default 5s timeout, non-negated.
    pub fn new(page: Page) -> Self {
        Self {
            page,
            timeout: DEFAULT_TIMEOUT,
            negated: false,
        }
    }

    /// Builder: override the assertion timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the assertion timeout in place.
    pub fn set_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    /// Return a negated clone: succeeding when the base condition is false.
    pub fn not(&self) -> Self {
        let mut c = self.clone();
        c.negated = true;
        c
    }

    /// Assert the page title equals `expected`.
    pub async fn to_have_title(&self, expected: &str) -> Result<()> {
        let p = self.page.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected.to_string();
        wait_for(
            move || {
                let p = p.clone();
                let expected_owned = expected_owned.clone();
                async move { Ok(p.title().await? == expected_owned) }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_title('{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the page URL equals `expected`.
    pub async fn to_have_url(&self, expected: &str) -> Result<()> {
        let p = self.page.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected.to_string();
        wait_for(
            move || {
                let p = p.clone();
                let expected_owned = expected_owned.clone();
                async move { Ok(p.url().await? == expected_owned) }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_url('{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }
}
