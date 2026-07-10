//! `Clock` — Playwright-style fake timers for a page.
//!
//! CDP has no native fake-timer primitive, so this is implemented by injecting a
//! compact fake-timer controller script into the page's main world and then
//! driving it with real `Runtime.evaluate` calls. The controller overrides
//! `Date` (so `new Date()` and `Date.now()` read from an adjustable base time),
//! `performance.now`, and the timer functions (`setTimeout`, `clearTimeout`,
//! `setInterval`, `clearInterval`, `requestAnimationFrame`).
//!
//! The shape mirrors Playwright's `page.clock()`:
//! - [`Clock::set_fixed_time`] pins time so it never advances;
//! - [`Clock::install`] engages the fakes (call before the code under test runs);
//! - [`Clock::advance`] / [`Clock::tick`] move the virtual clock forward,
//!   firing any due timers in between.
//!
//! # Scope
//! `install()` injects the controller on the *current* execution context only.
//! It is not automatically re-injected on cross-origin navigations; call
//! `install()` again after such a navigation. The injected controller lives on
//! the page's main world, so timers scheduled in isolated worlds (e.g. content
//! scripts) are not faked.

use crate::cdp::session::CdpSession;
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A fake-timer handle for a [`Page`](crate::Page).
///
/// Cheaply cloneable; all clones share the same page session and controller.
#[derive(Clone)]
pub struct Clock {
    inner: Arc<ClockInner>,
}

struct ClockInner {
    /// The page-level CDP session (Runtime/Emulation live here).
    session: CdpSession,
}

/// Options for [`Clock::install`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ClockInstallOptions {
    /// Wall-clock time (ms since the epoch) the fake clock starts at. If
    /// absent, the real current time at `install()` is used.
    pub time: Option<i64>,
    /// If `Some(true)`, the clock is fixed at this time (never auto-advances)
    /// after install — equivalent to also calling [`Clock::set_fixed_time`].
    pub fixed: Option<bool>,
}

impl ClockInstallOptions {
    pub fn time(mut self, v: i64) -> Self {
        self.time = Some(v);
        self
    }
    pub fn fixed(mut self, v: bool) -> Self {
        self.fixed = Some(v);
        self
    }
}

impl Clock {
    pub(crate) fn new(session: CdpSession) -> Self {
        Self {
            inner: Arc::new(ClockInner { session }),
        }
    }

    /// Engage the fake timers: installs the controller over `Date`,
    /// `performance.now`, and the timer functions, then optionally pins the
    /// time. Call this *before* the code under test schedules any timers.
    pub async fn install(&self, options: Option<ClockInstallOptions>) -> Result<()> {
        let opts = options.unwrap_or_default();
        let now_ms = opts.time.unwrap_or_else(current_unix_ms);

        // Inject the controller (idempotent: it no-ops if already installed).
        let _: Value = self
            .inner
            .session
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": INJECT_SCRIPT,
                    "awaitPromise": false,
                }),
            )
            .await?;

        // Seed the clock at the requested base time.
        let _: Value = self
            .inner
            .session
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": format!("(globalThis.__pwcdpClock && globalThis.__pwcdpClock.setNow({now_ms}))"),
                    "awaitPromise": false,
                }),
            )
            .await?;

        if opts.fixed.unwrap_or(false) {
            self.set_fixed_time(Some(now_ms)).await?;
        }
        Ok(())
    }

    /// Pin the virtual clock so `Date.now()`/`new Date()`/`performance.now()`
    /// stop advancing. If `time` is `Some(ms)`, the clock is also moved there
    /// first; `None` fixes it at the current virtual time.
    ///
    /// Calling this implicitly installs the controller if it is not present.
    pub async fn set_fixed_time(&self, time: Option<i64>) -> Result<()> {
        // Ensure the controller exists (no-op if already installed).
        let _: Value = self
            .inner
            .session
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": INJECT_SCRIPT,
                    "awaitPromise": false,
                }),
            )
            .await?;

        let expr = match time {
            Some(t) => format!(
                "(globalThis.__pwcdpClock && globalThis.__pwcdpClock.setFixed({t}))"
            ),
            None => "(globalThis.__pwcdpClock && globalThis.__pwcdpClock.setFixed())"
                .to_string(),
        };
        let _: Value = self
            .inner
            .session
            .send(
                "Runtime.evaluate",
                json!({ "expression": expr, "awaitPromise": false }),
            )
            .await?;
        Ok(())
    }

    /// Advance the virtual clock by `ms` milliseconds, firing every timer that
    /// falls due along the way (just like letting real time pass). Mirrors
    /// Playwright's `clock.advance`.
    pub async fn advance(&self, ms: i64) -> Result<()> {
        if ms < 0 {
            return Err(Error::InvalidArgument(format!(
                "Clock::advance expects a non-negative duration, got {ms}"
            )));
        }
        self.eval_controller_op(&format!("advance({ms})")).await
    }

    /// Advance the virtual clock by `ms` milliseconds **without** firing any
    /// timers in between (they remain pending). Mirrors Playwright's
    /// `clock.tick` short-form (duration only).
    pub async fn tick(&self, ms: i64) -> Result<()> {
        if ms < 0 {
            return Err(Error::InvalidArgument(format!(
                "Clock::tick expects a non-negative duration, got {ms}"
            )));
        }
        self.eval_controller_op(&format!("tick({ms})")).await
    }

    /// Run all pending timers (microtasks/timers queued up to "now") without
    /// moving the clock. Mirrors Playwright's `clock.run_all`.
    pub async fn run_all(&self) -> Result<()> {
        self.eval_controller_op("runAll()").await
    }

    /// Resume automatic (real) time progression on this page and clear all
    /// fakes. Pending fake timers are discarded. Mirrors Playwright's
    /// `clock.resume`.
    pub async fn resume(&self) -> Result<()> {
        self.eval_controller_op("resume()").await
    }

    /// Alias for [`Clock::resume`].
    pub async fn resume_all(&self) -> Result<()> {
        self.resume().await
    }

    /// Evaluate `globalThis.__pwcdpClock.<op>` against the controller, mapping a
    /// missing controller to a clear protocol error (the caller should have
    /// `install()`ed first).
    async fn eval_controller_op(&self, op: &str) -> Result<()> {
        let resp: Value = self
            .inner
            .session
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": format!("globalThis.__pwcdpClock && globalThis.__pwcdpClock.{op}"),
                    "awaitPromise": false,
                }),
            )
            .await?;

        // The expression evaluates to `false` when the controller was never
        // installed (the `&&` short-circuits). Surface that as a clear error.
        let installed = resp
            .get("result")
            .and_then(|r| r.get("value"))
            .map(|v| v.as_bool().unwrap_or(true))
            .unwrap_or(true);
        if !installed {
            return Err(Error::ProtocolError(
                "Clock controller is not installed; call Clock::install() first".into(),
            ));
        }
        Ok(())
    }
}

/// Current wall-clock time in milliseconds since the UNIX epoch.
fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The injected fake-timer controller. Defines `globalThis.__pwcdpClock` with:
/// `setNow(ms)`, `setFixed(ms?)`, `advance(ms)`, `tick(ms)`, `runAll()`,
/// `resume()`.
///
/// `fixed` means the clock never moves (Date.now() always returns the base);
/// otherwise it ticks in real time off the base plus elapsed wall time, which
/// matches Playwright's "loose" timer semantics. `advance(ms)` jumps forward
/// and drains every timer whose deadline is now in the past, in deadline order.
/// `tick(ms)` only moves the base forward (timers stay pending).
const INJECT_SCRIPT: &str = r#"(function(){
  if (globalThis.__pwcdpClock) return true;
  var g = globalThis;
  var origDate = g.Date;
  // Capture the REAL Date.now before it is overridden, otherwise
  // virtualNow()/realAtInstall would recurse into FakeDate.now (stack overflow).
  var origDateNow = g.Date.now.bind(g.Date);
  var origNow = (typeof performance !== 'undefined' && performance.now) ? performance.now.bind(performance) : null;
  var origSetTimeout = g.setTimeout, origClearTimeout = g.clearTimeout;
  var origSetInterval = g.setInterval, origClearInterval = g.clearInterval;
  var origRAF = g.requestAnimationFrame, origCancelRAF = g.cancelAnimationFrame;

  var installed = false;
  var fixed = false;
  var base = origDateNow();
  var realAtInstall = origDateNow();
  var nextId = 1;
  var timers = {};

  function virtualNow() {
    if (fixed) return base;
    return base + (origDateNow() - realAtInstall);
  }

  function FakeDate() {
    if (!(this instanceof FakeDate)) return new origDate(virtualNow());
    if (arguments.length === 0) return new origDate(virtualNow());
    return new (Function.prototype.bind.apply(origDate, [null].concat([].slice.call(arguments))));
  }
  FakeDate.prototype = origDate.prototype;
  FakeDate.now = function(){ return virtualNow(); };
  FakeDate.parse = origDate.parse;
  FakeDate.UTC = origDate.UTC;
  FakeDate.toString = function(){ return origDate.toString(); };

  function fakeSetTimeout(cb, delay) {
    var id = nextId++;
    var d = Math.max(0, Number(delay) || 0);
    var args = [].slice.call(arguments, 2);
    timers[id] = { fireAt: virtualNow() + d, interval: null, cb: cb, args: args };
    return id;
  }
  function fakeSetInterval(cb, delay) {
    var id = nextId++;
    var d = Math.max(0, Number(delay) || 0);
    var args = [].slice.call(arguments, 2);
    timers[id] = { fireAt: virtualNow() + d, interval: d, cb: cb, args: args };
    return id;
  }
  function fakeClear(id) {
    if (id != null) delete timers[id];
  }
  function fakeRAF(cb) {
    return fakeSetTimeout(function(){ cb(virtualNow()); }, 16);
  }

  function installGlobals() {
    if (installed) return;
    g.Date = FakeDate;
    g.setTimeout = fakeSetTimeout;
    g.clearTimeout = fakeClear;
    g.setInterval = fakeSetInterval;
    g.clearInterval = fakeClear;
    g.requestAnimationFrame = fakeRAF;
    g.cancelAnimationFrame = fakeClear;
    if (origNow && typeof performance !== 'undefined') {
      var startVirtual = virtualNow();
      performance.now = function(){ return (virtualNow() - startVirtual); };
    }
    installed = true;
  }
  function restoreGlobals() {
    if (!installed) return;
    g.Date = origDate;
    g.setTimeout = origSetTimeout;
    g.clearTimeout = origClearTimeout;
    g.setInterval = origSetInterval;
    g.clearInterval = origClearInterval;
    g.requestAnimationFrame = origRAF;
    g.cancelAnimationFrame = origCancelRAF;
    if (origNow && typeof performance !== 'undefined') performance.now = origNow;
    installed = false;
  }

  function drain(upto) {
    var guard = 0;
    var fired = true;
    while (fired && guard < 100000) {
      fired = false;
      guard++;
      var due = [];
      for (var id in timers) {
        if (timers[id].fireAt <= upto) due.push([Number(id), timers[id].fireAt]);
      }
      if (due.length === 0) break;
      due.sort(function(a,b){ return a[1] - b[1]; });
      var pick = due[0][0];
      var t = timers[pick];
      if (!t) continue;
      try { t.cb.apply(null, t.args || []); } catch (e) { /* swallow */ }
      fired = true;
      if (t.interval != null) {
        t.fireAt = upto + t.interval - ((upto - t.fireAt) % (t.interval || 1));
      } else {
        delete timers[pick];
      }
    }
  }

  g.__pwcdpClock = {
    setNow: function(ms){ base = Number(ms) || 0; realAtInstall = origDateNow(); return true; },
    setFixed: function(ms){
      if (typeof ms === 'number') { base = ms; realAtInstall = origDateNow(); }
      fixed = true;
      installGlobals();
      return true;
    },
    advance: function(ms){
      installGlobals();
      fixed = false;
      var target = virtualNow() + (Number(ms) || 0);
      base = target;
      realAtInstall = origDateNow();
      drain(target);
      return true;
    },
    tick: function(ms){
      installGlobals();
      fixed = false;
      base = virtualNow() + (Number(ms) || 0);
      realAtInstall = origDateNow();
      return true;
    },
    runAll: function(){
      installGlobals();
      fixed = false;
      drain(virtualNow());
      return true;
    },
    resume: function(){
      restoreGlobals();
      fixed = false;
      timers = {};
      base = origDateNow();
      realAtInstall = origDateNow();
      return true;
    }
  };
  return true;
})();
"#;
