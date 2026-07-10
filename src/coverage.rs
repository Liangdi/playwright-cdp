//! `Coverage` — JS and CSS code-coverage collection for a page.
//!
//! Speaks CDP's coverage domains directly on the page session:
//! - **JS coverage** via the `Profiler` domain:
//!   `Profiler.startPreciseCoverage` (with `callCount` + `detailed`) on start,
//!   `Profiler.takePreciseCoverage` then `Profiler.stopPreciseCoverage` on stop.
//! - **CSS coverage** via the `CSS` domain:
//!   `CSS.enable` + `CSS.startRuleUsageTracking` on start, and
//!   `CSS.stopRuleUsageTracking` (which returns rule usage) on stop.
//!
//! Mirrors the shape of Playwright's `page.coverage()`.

use crate::cdp::session::CdpSession;
use crate::error::Result;
use serde_json::{json, Value};
use std::sync::Arc;

/// A code-coverage handle for a [`Page`](crate::Page).
///
/// Cheaply cloneable; all clones share the same page session.
#[derive(Clone)]
pub struct Coverage {
    inner: Arc<CoverageInner>,
}

struct CoverageInner {
    /// The page-level CDP session (Profiler/CSS live here).
    session: CdpSession,
}

/// Options for [`Coverage::start_js_coverage`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct JSCoverageStartOptions {
    /// Report anonymous (non-covered vs covered byte ranges) as well as
    /// call counts. Maps to CDP `detailed`. Defaults to `true`.
    pub report_anonymous: Option<bool>,
    /// Collect per-block coverage (true) rather than per-function (false).
    /// Maps to CDP `detailed`. Defaults to `true`.
    pub include_block_coverage: Option<bool>,
}

impl JSCoverageStartOptions {
    pub fn report_anonymous(mut self, v: bool) -> Self {
        self.report_anonymous = Some(v);
        self
    }
    pub fn include_block_coverage(mut self, v: bool) -> Self {
        self.include_block_coverage = Some(v);
        self
    }
}

/// A single script reported by JS coverage.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct JSCoverageEntry {
    /// The script's URL (may be empty for inline/eval'd scripts).
    pub url: String,
    /// The CDP script id.
    pub script_id: String,
    /// Per-function coverage records.
    pub functions: Vec<JSCoverageFunction>,
}

/// A single function within a JS coverage entry.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct JSCoverageFunction {
    /// Function name (may be empty for anonymous functions).
    pub function_name: String,
    /// Whether block-level coverage was collected for this function.
    pub is_block_coverage: bool,
    /// Covered ranges: byte offsets and call counts.
    pub ranges: Vec<JSCoverageRange>,
}

/// A covered byte range inside a JS function.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct JSCoverageRange {
    /// Inclusive start byte offset within the script source.
    pub start_offset: i64,
    /// Exclusive end byte offset within the script source.
    pub end_offset: i64,
    /// Number of times this range was executed.
    pub call_count: i64,
}

/// The result of [`Coverage::stop_js_coverage`]: one entry per script.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct JSCoverageResult {
    /// One entry per covered script.
    pub entries: Vec<JSCoverageEntry>,
}

/// A single CSS style sheet reported by CSS coverage.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CSSCoverageEntry {
    /// The style sheet id.
    pub style_sheet_id: String,
    /// Per-rule usage records.
    pub rules: Vec<CSSCoverageRule>,
}

/// A single CSS rule's coverage record.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CSSCoverageRule {
    /// The owning style sheet id.
    pub style_sheet_id: String,
    /// Inclusive start byte offset within the sheet source.
    pub start_offset: i64,
    /// Exclusive end byte offset within the sheet source.
    pub end_offset: i64,
    /// Whether the rule was used during the capture.
    pub used: bool,
}

/// The result of [`Coverage::stop_css_coverage`]: rule usage grouped by sheet.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CSSCoverageResult {
    /// One entry per style sheet with usage data.
    pub entries: Vec<CSSCoverageEntry>,
}

impl Coverage {
    pub(crate) fn new(session: CdpSession) -> Self {
        Self {
            inner: Arc::new(CoverageInner { session }),
        }
    }

    // --- JS coverage (Profiler domain) -------------------------------------

    /// Begin collecting precise JS coverage. Enables the `Profiler` domain,
    /// then issues `Profiler.startPreciseCoverage { callCount: true,
    /// detailed: <opts> }`.
    ///
    /// `Profiler.enable` must precede `startPreciseCoverage` — CDP rejects
    /// the latter with `Profiler is not enabled` otherwise.
    pub async fn start_js_coverage(
        &self,
        options: Option<JSCoverageStartOptions>,
    ) -> Result<()> {
        self.inner.session.send("Profiler.enable", json!({})).await?;
        let opts = options.unwrap_or_default();
        let detailed = opts.include_block_coverage.unwrap_or(true);
        self.inner
            .session
            .send(
                "Profiler.startPreciseCoverage",
                json!({ "callCount": true, "detailed": detailed }),
            )
            .await?;
        Ok(())
    }

    /// Stop collecting JS coverage and return the accumulated report. Issues
    /// `Profiler.takePreciseCoverage` (the result) then
    /// `Profiler.stopPreciseCoverage` (teardown).
    pub async fn stop_js_coverage(&self) -> Result<JSCoverageResult> {
        let resp: Value = self
            .inner
            .session
            .send("Profiler.takePreciseCoverage", json!({}))
            .await?;

        // Tear down precise coverage and disable the Profiler domain to restore
        // baseline state, regardless of parse success.
        let _ = self
            .inner
            .session
            .send("Profiler.stopPreciseCoverage", json!({}))
            .await;
        let _ = self.inner.session.send("Profiler.disable", json!({})).await;

        let entries = resp
            .get("result")
            .and_then(|v| v.as_array())
            .map(|a| parse_js_entries(a))
            .unwrap_or_default();

        Ok(JSCoverageResult { entries })
    }

    // --- CSS coverage (CSS domain) -----------------------------------------

    /// Begin collecting CSS rule usage tracking. Enables the `CSS` domain and
    /// issues `CSS.startRuleUsageTracking`.
    pub async fn start_css_coverage(&self) -> Result<()> {
        self.inner.session.send("CSS.enable", json!({})).await?;
        self.inner
            .session
            .send("CSS.startRuleUsageTracking", json!({}))
            .await?;
        Ok(())
    }

    /// Stop collecting CSS coverage and return rule usage. Issues
    /// `CSS.stopRuleUsageTracking` (which returns the usage list), then
    /// disables the `CSS` domain.
    pub async fn stop_css_coverage(&self) -> Result<CSSCoverageResult> {
        let resp: Value = self
            .inner
            .session
            .send("CSS.stopRuleUsageTracking", json!({}))
            .await?;

        // Disable CSS domain to restore baseline state.
        let _ = self.inner.session.send("CSS.disable", json!({})).await;

        let rules = resp
            .get("ruleUsage")
            .and_then(|v| v.as_array())
            .map(|a| parse_css_rules(a))
            .unwrap_or_default();

        // Group the flat rule-usage list by style sheet id.
        let mut entries: Vec<CSSCoverageEntry> = Vec::new();
        for rule in rules {
            if let Some(e) = entries.iter_mut().find(|e| e.style_sheet_id == rule.style_sheet_id)
            {
                e.rules.push(rule);
            } else {
                entries.push(CSSCoverageEntry {
                    style_sheet_id: rule.style_sheet_id.clone(),
                    rules: vec![rule],
                });
            }
        }

        Ok(CSSCoverageResult { entries })
    }
}

/// Parse the CDP `Profiler.takePreciseCoverage.result` array into entries.
fn parse_js_entries(arr: &[Value]) -> Vec<JSCoverageEntry> {
    arr.iter()
        .filter_map(|s| {
            let url = s.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let script_id = s
                .get("scriptId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let functions = s
                .get("functions")
                .and_then(|v| v.as_array())
                .map(|fs| fs.iter().filter_map(parse_js_function).collect())
                .unwrap_or_default();
            Some(JSCoverageEntry {
                url,
                script_id,
                functions,
            })
        })
        .collect()
}

/// Parse one CDP coverage function record.
fn parse_js_function(f: &Value) -> Option<JSCoverageFunction> {
    let function_name = f
        .get("functionName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let is_block_coverage = f
        .get("isBlockCoverage")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ranges = f
        .get("ranges")
        .and_then(|v| v.as_array())
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    Some(JSCoverageRange {
                        start_offset: r.get("startOffset").and_then(|v| v.as_i64())?,
                        end_offset: r.get("endOffset").and_then(|v| v.as_i64())?,
                        call_count: r.get("callCount").and_then(|v| v.as_i64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(JSCoverageFunction {
        function_name,
        is_block_coverage,
        ranges,
    })
}

/// Parse the CDP `CSS.stopRuleUsageTracking.ruleUsage` array into rule records.
fn parse_css_rules(arr: &[Value]) -> Vec<CSSCoverageRule> {
    arr.iter()
        .filter_map(|r| {
            Some(CSSCoverageRule {
                style_sheet_id: r.get("styleSheetId").and_then(|v| v.as_str())?.to_string(),
                start_offset: r.get("startOffset").and_then(|v| v.as_i64())?,
                end_offset: r.get("endOffset").and_then(|v| v.as_i64())?,
                used: r.get("used").and_then(|v| v.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}
