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
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::locator::Locator;
use crate::page::Page;

/// Default assertion timeout (mirrors Playwright's `expect` default).
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);
/// Polling interval between condition checks.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

// ===========================================================================
// Screenshot assertion options + pixel-tolerance comparison (feature-gated)
// ===========================================================================

/// Options for [`LocatorAssertions::to_have_screenshot`].
///
/// Builder-style; constructed via [`ScreenshotAssertOptions::new`] then chained
/// setters. All fields are optional and default to a strict comparison
/// (`max_diff_pixels = 0`, i.e. zero tolerated differing pixels).
///
/// **Only effective when the `screenshot-diff` cargo feature is enabled.**
/// With the feature OFF (default), `to_have_screenshot` performs exact
/// byte comparison and these options are ignored.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ScreenshotAssertOptions {
    /// Maximum number of differing pixels allowed for the assertion to pass
    /// (default `0` — strict). Only consulted when `screenshot-diff` is on.
    pub max_diff_pixels: Option<u64>,
    /// Per-pixel color-distance threshold in `0.0..=1.0`. Two pixels whose
    /// normalized RGBA distance exceeds this count as "different" (default
    /// `0.0` — pixels must be byte-identical to be considered equal). Only
    /// consulted when `screenshot-diff` is on.
    pub threshold: Option<f32>,
}

impl ScreenshotAssertOptions {
    /// Create an empty (all-default) options set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of differing pixels tolerated.
    pub fn with_max_diff_pixels(mut self, pixels: u64) -> Self {
        self.max_diff_pixels = Some(pixels);
        self
    }

    /// Set the per-pixel color-distance threshold (`0.0..=1.0`).
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = Some(threshold);
        self
    }
}

/// Write `actual` bytes to `{path-stem}-actual.png` next to the baseline.
/// `path` is the baseline path ending in `.png`.
fn write_actual(path: &str, actual: &[u8]) -> Result<()> {
    let mut actual_path = path.to_string();
    actual_path.truncate(actual_path.len() - ".png".len());
    actual_path.push_str("-actual.png");
    std::fs::write(&actual_path, actual)?;
    Ok(())
}

/// Outcome of comparing two decoded screenshots.
#[cfg(feature = "screenshot-diff")]
enum ScreenshotCmp {
    /// Pixels are within tolerance.
    Match,
    /// Pixels exceed tolerance. `diff` is the differing-pixel count, `total`
    /// the total pixel count of the (matched-size) image.
    Mismatch { diff: u64, total: u64 },
}

/// Decode two PNG byte buffers and compare them pixel-by-pixel with the
/// tolerance in `opts`. Returns [`ScreenshotCmp::Match`] when within tolerance.
///
/// - Different dimensions -> always a mismatch (`diff = total` of the larger).
/// - A pixel counts as "different" when its normalized RGBA Euclidean distance
///   exceeds `threshold` (default `0.0`, i.e. any channel difference).
/// - The comparison passes when `diff <= max_diff_pixels` (default `0`).
#[cfg(feature = "screenshot-diff")]
fn compare_screenshot_pixels(
    baseline: &[u8],
    actual: &[u8],
    opts: &ScreenshotAssertOptions,
) -> Result<ScreenshotCmp> {
    use image::GenericImageView;

    let baseline_img = image::load_from_memory(baseline)
        .map_err(|e| Error::ProtocolError(format!("failed to decode baseline image: {e}")))?;
    let actual_img = image::load_from_memory(actual)
        .map_err(|e| Error::ProtocolError(format!("failed to decode actual image: {e}")))?;

    let (bw, bh) = baseline_img.dimensions();
    let (aw, ah) = actual_img.dimensions();

    // Different dimensions -> never a match.
    if bw != aw || bh != ah {
        let total = (bw as u64).max(aw as u64) * (bh as u64).max(ah as u64);
        return Ok(ScreenshotCmp::Mismatch {
            diff: total,
            total,
        });
    }

    let total = bw as u64 * bh as u64;
    if total == 0 {
        return Ok(ScreenshotCmp::Match);
    }

    let threshold = opts.threshold.unwrap_or(0.0) as f64;
    let threshold_sq = threshold * threshold;
    let mut diff: u64 = 0;

    for y in 0..bh {
        for x in 0..bw {
            let bp = baseline_img.get_pixel(x, y);
            let ap = actual_img.get_pixel(x, y);
            // Normalized per-channel distance (each channel 0.0-1.0).
            let dr = (i32::from(bp[0]) - i32::from(ap[0])) as f64 / 255.0;
            let dg = (i32::from(bp[1]) - i32::from(ap[1])) as f64 / 255.0;
            let db = (i32::from(bp[2]) - i32::from(ap[2])) as f64 / 255.0;
            let da = (i32::from(bp[3]) - i32::from(ap[3])) as f64 / 255.0;
            let dist_sq = (dr * dr + dg * dg + db * db + da * da) / 4.0;
            if dist_sq > threshold_sq {
                diff += 1;
            }
        }
    }

    let max_diff = opts.max_diff_pixels.unwrap_or(0);
    if diff <= max_diff {
        Ok(ScreenshotCmp::Match)
    } else {
        Ok(ScreenshotCmp::Mismatch { diff, total })
    }
}

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

    // --- text (regex variants) ---

    /// Assert the element's trimmed `textContent` matches the regex `pattern`.
    pub async fn to_have_text_regex(&self, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                async move {
                    let actual = l.text_content().await?;
                    let actual = actual.unwrap_or_default();
                    Ok(re.is_match(actual.trim()))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_text_regex('{sel}', '{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's `textContent` matches the regex `pattern` (no trim).
    pub async fn to_contain_text_regex(&self, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                async move {
                    let actual = l.text_content().await?;
                    let actual = actual.unwrap_or_default();
                    Ok(re.is_match(&actual))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_contain_text_regex('{sel}', '{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's `innerText` matches the regex `pattern`.
    pub async fn to_have_inner_text_regex(&self, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                async move {
                    let actual = l.inner_text().await?;
                    Ok(re.is_match(&actual))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_inner_text_regex('{sel}', '{pattern_for_msg}') timed out after {}ms",
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

    /// Assert the element's input value matches the regex `pattern`.
    pub async fn to_have_value_regex(&self, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                async move {
                    let actual = l.input_value(None).await?;
                    Ok(re.is_match(&actual))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_value_regex('{sel}', '{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's `class` attribute matches the regex `pattern`.
    pub async fn to_have_attribute_regex(&self, name: &str, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let name_owned = name.to_string();
        let name_for_msg = name.to_string();
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                let name_owned = name_owned.clone();
                async move {
                    let attr = l.get_attribute(&name_owned).await?;
                    Ok(match attr {
                        Some(v) => re.is_match(&v),
                        None => false,
                    })
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_attribute_regex('{sel}', '{name_for_msg}', '{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    // --- CSS (computed style) ---

    /// Assert the element's computed CSS property `name` equals `value`.
    ///
    /// Reads the value via `getComputedStyle(el).getPropertyValue(name)` and
    /// compares for exact equality.
    pub async fn to_have_css(&self, name: &str, value: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expr = format!(
            "getComputedStyle(el).getPropertyValue({})",
            serde_json::to_string(name)?
        );
        let value_owned = value.to_string();
        let name_for_msg = name.to_string();
        let value_for_msg = value.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let expr = expr.clone();
                let value_owned = value_owned.clone();
                async move {
                    let actual: String = l.evaluate(&expr, None).await?;
                    Ok(actual == value_owned)
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_css('{sel}', '{name_for_msg}', '{value_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's computed CSS property `name` matches the regex
    /// `pattern`.
    pub async fn to_have_css_regex(&self, name: &str, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let expr = format!(
            "getComputedStyle(el).getPropertyValue({})",
            serde_json::to_string(name)?
        );
        let name_for_msg = name.to_string();
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                let expr = expr.clone();
                async move {
                    let actual: String = l.evaluate(&expr, None).await?;
                    Ok(re.is_match(&actual))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_css_regex('{sel}', '{name_for_msg}', '{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    // --- class ---

    /// Assert the element's `class` attribute equals `expected`.
    pub async fn to_have_class(&self, expected: &str) -> Result<()> {
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
                async move {
                    let class = l.get_attribute("class").await?;
                    Ok(class.unwrap_or_default() == expected_owned)
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_class('{sel}', '{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the element's `class` attribute matches the regex `pattern`.
    pub async fn to_have_class_regex(&self, pattern: &str) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let l = l.clone();
                let re = re.clone();
                async move {
                    let class = l.get_attribute("class").await?;
                    Ok(match class {
                        Some(v) => re.is_match(&v),
                        None => false,
                    })
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_class_regex('{sel}', '{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    // --- focus ---

    /// Assert the element is the current `document.activeElement`.
    pub async fn to_be_focused(&self) -> Result<()> {
        let sel = self.sel().to_string();
        let l = self.locator.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        wait_for(
            move || {
                let l = l.clone();
                async move { l.is_focused().await }
            },
            timeout,
            negated,
            || format!("to_be_focused('{sel}') timed out after {}ms", timeout.as_millis()),
        )
        .await
    }

    // --- ARIA snapshot ---

    /// Assert the element's aria snapshot equals `expected`.
    ///
    /// Simplified variant: compares the trimmed snapshot string against the
    /// trimmed `expected` for exact equality. This is **not** Playwright's
    /// structured accessibility-tree diff (it does no node normalization or
    /// whitespace folding beyond `trim()`), so mismatches in indentation or
    /// ordering will fail even when the trees are semantically equivalent.
    pub async fn to_match_aria_snapshot(&self, expected: &str) -> Result<()> {
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
                async move {
                    let actual = l.aria_snapshot().await?;
                    Ok(actual.trim() == expected_owned.trim())
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_match_aria_snapshot('{sel}', '{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    // --- screenshot ---

    /// Assert the element's screenshot matches a baseline PNG file `name`.
    ///
    /// A `.png` suffix is appended to `name` if absent. If the baseline does
    /// not yet exist, the first run writes it (establishing the baseline) and
    /// succeeds.
    ///
    /// **Comparison mode depends on the `screenshot-diff` cargo feature:**
    ///
    /// - **Feature OFF (default):** exact byte-level comparison of the
    ///   captured PNG against the baseline. The rendered bytes must be
    ///   identical. The `options` argument is accepted but ignored.
    /// - **Feature ON (`screenshot-diff`):** pixel-level tolerance comparison
    ///   via the `image` crate. Both PNGs are decoded to RGBA and compared
    ///   pixel-by-pixel; a mismatch is reported only when the number of
    ///   differing pixels exceeds the tolerance in [`ScreenshotAssertOptions`]
    ///   (`max_diff_pixels` / `threshold`). Disable the feature to drop the
    ///   `image` dependency entirely.
    ///
    /// On mismatch the actual bytes are always dumped to `{name}-actual.png`
    /// for inspection, and the error message carries diff statistics.
    ///
    /// Because this captures a single frame rather than polling a condition,
    /// it bypasses [`wait_for`] and runs to completion immediately.
    pub async fn to_have_screenshot(
        &self,
        name: &str,
        options: Option<ScreenshotAssertOptions>,
    ) -> Result<()> {
        let actual = self.locator.screenshot(None).await?;
        let mut path = name.to_string();
        if Path::new(&path).extension().and_then(|e| e.to_str()) != Some("png") {
            path.push_str(".png");
        }
        if !Path::new(&path).exists() {
            std::fs::write(&path, &actual)?;
            return Ok(());
        }
        let baseline = std::fs::read(&path)?;

        #[cfg(feature = "screenshot-diff")]
        {
            let opts = options.unwrap_or_default();
            match compare_screenshot_pixels(&baseline, &actual, &opts)? {
                ScreenshotCmp::Match => Ok(()),
                ScreenshotCmp::Mismatch { diff, total } => {
                    write_actual(&path, &actual)?;
                    Err(Error::Timeout(format!(
                        "to_have_screenshot('{}') does not match baseline: {} of {} pixels differ \
                         (max_diff_pixels={}, threshold={}); actual written to '{}-actual.png'",
                        path,
                        diff,
                        total,
                        opts.max_diff_pixels.unwrap_or(0),
                        opts.threshold.unwrap_or(0.0),
                        path.trim_end_matches(".png"),
                    )))
                }
            }
        }

        #[cfg(not(feature = "screenshot-diff"))]
        {
            // No options consumed in byte mode; silence unused-variable lint.
            let _ = options;
            if baseline == actual {
                Ok(())
            } else {
                write_actual(&path, &actual)?;
                Err(Error::Timeout(format!(
                    "to_have_screenshot('{}') does not match baseline; actual written to '{}-actual.png'",
                    path,
                    path.trim_end_matches(".png")
                )))
            }
        }
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

    /// Assert the page title matches the regex `pattern`.
    pub async fn to_have_title_regex(&self, pattern: &str) -> Result<()> {
        let p = self.page.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let p = p.clone();
                let re = re.clone();
                async move {
                    let actual = p.title().await?;
                    Ok(re.is_match(&actual))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_title_regex('{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the page URL matches the regex `pattern`.
    pub async fn to_have_url_regex(&self, pattern: &str) -> Result<()> {
        let p = self.page.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let re = regex::Regex::new(pattern)
            .map_err(|e| Error::InvalidArgument(format!("invalid regex pattern {pattern:?}: {e}")))?;
        let pattern_for_msg = pattern.to_string();
        wait_for(
            move || {
                let p = p.clone();
                let re = re.clone();
                async move {
                    let actual = p.url().await?;
                    Ok(re.is_match(&actual))
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_have_url_regex('{pattern_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }

    /// Assert the page's aria snapshot equals `expected`.
    ///
    /// Simplified variant: compares the trimmed snapshot string against the
    /// trimmed `expected` for exact equality — **not** Playwright's structured
    /// accessibility-tree diff.
    pub async fn to_match_aria_snapshot(&self, expected: &str) -> Result<()> {
        let p = self.page.clone();
        let negated = self.negated;
        let timeout = self.timeout;
        let expected_owned = expected.to_string();
        let expected_for_msg = expected.to_string();
        wait_for(
            move || {
                let p = p.clone();
                let expected_owned = expected_owned.clone();
                async move {
                    let actual = p.aria_snapshot().await?;
                    Ok(actual.trim() == expected_owned.trim())
                }
            },
            timeout,
            negated,
            move || {
                format!(
                    "to_match_aria_snapshot('{expected_for_msg}') timed out after {}ms",
                    timeout.as_millis()
                )
            },
        )
        .await
    }
}
