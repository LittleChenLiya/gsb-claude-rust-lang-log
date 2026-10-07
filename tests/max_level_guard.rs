// Per-level counting helpers are compiled out when a static max level
// Cargo feature filters records before they reach the logger.
#![allow(dead_code, unused_imports)]

//! Acceptance tests for scoped maximum-level guards
//! (`log::set_max_level_scoped` / `log::MaxLevelGuard`).
//!
//! A single in-memory counting logger is registered once (registration state
//! never changes afterwards) and the three required behaviors are exercised
//! on one thread:
//!
//! 1. nested guards restore in stack order on scope exit;
//! 2. an explicit `set_max_level` update is never overwritten by a stale
//!    guard;
//! 3. a guard dropped while unwinding a panic still performs its valid
//!    restoration.
//!
//! The recorded log counts are checked against the global filter semantics
//! and the logger registration is asserted unchanged at the end.

use log::{debug, error, info, trace, warn, LevelFilter, Log, Metadata, Record};
use std::sync::{Mutex, OnceLock};

// Per-level counts are only meaningful when no static max level feature
// filters records before they reach the logger. Level-state and registration
// assertions are still valid with those features.
#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
use std::panic::{self, AssertUnwindSafe};

/// Number of records that reached the logger, broken down by level.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    error: usize,
    warn: usize,
    info: usize,
    debug: usize,
    trace: usize,
}

impl Counts {
    #[cfg(not(any(
        feature = "max_level_off",
        feature = "max_level_error",
        feature = "max_level_warn",
        feature = "max_level_info",
        feature = "max_level_debug",
        feature = "max_level_trace",
        feature = "release_max_level_off",
        feature = "release_max_level_error",
        feature = "release_max_level_warn",
        feature = "release_max_level_info",
        feature = "release_max_level_debug",
        feature = "release_max_level_trace",
    )))]
    fn total(&self) -> usize {
        self.error + self.warn + self.info + self.debug + self.trace
    }
}

/// A minimal logger that only counts records, one for every level.
struct CountingLogger {
    counts: Mutex<Counts>,
}

impl CountingLogger {
    const fn new() -> Self {
        CountingLogger {
            counts: Mutex::new(Counts {
                error: 0,
                warn: 0,
                info: 0,
                debug: 0,
                trace: 0,
            }),
        }
    }

    fn counts(&self) -> Counts {
        *self.counts.lock().unwrap()
    }

    #[cfg(not(any(
        feature = "max_level_off",
        feature = "max_level_error",
        feature = "max_level_warn",
        feature = "max_level_info",
        feature = "max_level_debug",
        feature = "max_level_trace",
        feature = "release_max_level_off",
        feature = "release_max_level_error",
        feature = "release_max_level_warn",
        feature = "release_max_level_info",
        feature = "release_max_level_debug",
        feature = "release_max_level_trace",
    )))]
    fn reset(&self) {
        *self.counts.lock().unwrap() = Counts::default();
    }
}

impl Log for CountingLogger {
    // The logger itself does no filtering; the global max level does.
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let mut counts = self.counts.lock().unwrap();
        match record.level() {
            log::Level::Error => counts.error += 1,
            log::Level::Warn => counts.warn += 1,
            log::Level::Info => counts.info += 1,
            log::Level::Debug => counts.debug += 1,
            log::Level::Trace => counts.trace += 1,
        }
    }

    fn flush(&self) {}
}

static LOGGER: CountingLogger = CountingLogger::new();

/// Registers the logger exactly once for the whole test binary.
fn init() -> &'static CountingLogger {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        log::set_logger(&LOGGER).expect("logger registration must succeed once");
        // `set_logger` does not touch the max level; start from `Trace` so
        // every record would pass without scoped/global filtering.
        log::set_max_level(LevelFilter::Trace);
    });
    &LOGGER
}

#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
/// Emits one record at each level and returns how many more records the
/// logger received.
fn log_one_of_each(logger: &CountingLogger) -> Counts {
    let before = logger.counts();
    error!("error record");
    warn!("warn record");
    info!("info record");
    debug!("debug record");
    trace!("trace record");
    let after = logger.counts();
    Counts {
        error: after.error - before.error,
        warn: after.warn - before.warn,
        info: after.info - before.info,
        debug: after.debug - before.debug,
        trace: after.trace - before.trace,
    }
}

#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
/// Expected per-level counts after emitting one record at every level: only
/// levels at or above `filter` survive the global filtering of the macros.
fn expected_counts(filter: LevelFilter) -> Counts {
    let passes = |level: LevelFilter| level <= filter;
    Counts {
        error: passes(LevelFilter::Error) as usize,
        warn: passes(LevelFilter::Warn) as usize,
        info: passes(LevelFilter::Info) as usize,
        debug: passes(LevelFilter::Debug) as usize,
        trace: passes(LevelFilter::Trace) as usize,
    }
}

#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
/// Asserts that emitting one record at each level yields exactly the counts
/// dictated by the currently active dynamic filter.
fn assert_filtered_counts(logger: &CountingLogger, filter: LevelFilter) {
    assert_eq!(
        log_one_of_each(logger),
        expected_counts(filter),
        "one record at each level filtered by {:?}",
        filter
    );
}

#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
/// 1. Nested guards restore the previous level in stack order on exit.
fn phase_nested_exit(logger: &CountingLogger) {
    logger.reset();
    log::set_max_level(LevelFilter::Info);

    // Baseline: Info lets error/warn/info through, filters debug/trace.
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert_filtered_counts(logger, LevelFilter::Info);

    // Outer scope narrows to Warn.
    let outer = log::set_max_level_scoped(LevelFilter::Warn);
    assert_eq!(log::max_level(), LevelFilter::Warn);
    assert_filtered_counts(logger, LevelFilter::Warn);

    // Inner scope narrows further to Off; nothing gets through.
    {
        let inner = log::set_max_level_scoped(LevelFilter::Off);
        assert_eq!(log::max_level(), LevelFilter::Off);
        assert_filtered_counts(logger, LevelFilter::Off);
        // Inner guard dropped here: Warn must be restored.
        drop(inner);
    }
    assert_eq!(log::max_level(), LevelFilter::Warn);
    assert_filtered_counts(logger, LevelFilter::Warn);

    // Outer guard dropped: original Info level is back.
    drop(outer);
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert_filtered_counts(logger, LevelFilter::Info);

    // Reset for later phases.
    log::set_max_level(LevelFilter::Trace);
}

#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
/// 2. An explicit `set_max_level` update cannot be overwritten by a stale
///    guard on drop.
fn phase_explicit_update_wins(logger: &CountingLogger) {
    logger.reset();
    log::set_max_level(LevelFilter::Info);

    let guard = log::set_max_level_scoped(LevelFilter::Trace);
    assert_eq!(log::max_level(), LevelFilter::Trace);
    assert_filtered_counts(logger, LevelFilter::Trace);

    // Explicit update while the guard is alive.
    log::set_max_level(LevelFilter::Error);
    assert_eq!(log::max_level(), LevelFilter::Error);

    // The stale guard must not restore Trace.
    drop(guard);
    assert_eq!(log::max_level(), LevelFilter::Error);
    assert_filtered_counts(logger, LevelFilter::Error);

    // The same rule applies to nested guards: an explicit update while an
    // inner guard is alive invalidates restoration of both guards, and
    // newly entered scopes snapshot the explicit value.
    log::set_max_level(LevelFilter::Info);
    let outer = log::set_max_level_scoped(LevelFilter::Debug);
    {
        let inner = log::set_max_level_scoped(LevelFilter::Off);
        assert_eq!(log::max_level(), LevelFilter::Off);
        log::set_max_level(LevelFilter::Warn);
        drop(inner);
    }
    assert_eq!(log::max_level(), LevelFilter::Warn);
    drop(outer);
    assert_eq!(log::max_level(), LevelFilter::Warn);

    // Even an explicit update to the *same* value is an update.
    log::set_max_level(LevelFilter::Info);
    let guard = log::set_max_level_scoped(LevelFilter::Info);
    log::set_max_level(LevelFilter::Info);
    drop(guard);
    assert_eq!(log::max_level(), LevelFilter::Info);

    log::set_max_level(LevelFilter::Trace);
}

#[cfg(not(any(
    feature = "max_level_off",
    feature = "max_level_error",
    feature = "max_level_warn",
    feature = "max_level_info",
    feature = "max_level_debug",
    feature = "max_level_trace",
    feature = "release_max_level_off",
    feature = "release_max_level_error",
    feature = "release_max_level_warn",
    feature = "release_max_level_info",
    feature = "release_max_level_debug",
    feature = "release_max_level_trace",
)))]
/// 3. Restoration still runs when the scope exits through a panic that is
///    caught with `catch_unwind`.
fn phase_panic_restores(logger: &CountingLogger) {
    logger.reset();
    log::set_max_level(LevelFilter::Warn);

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let inner = log::set_max_level_scoped(LevelFilter::Off);
        assert_eq!(log::max_level(), LevelFilter::Off);
        assert_filtered_counts(&logger, LevelFilter::Off);

        // Simulate an abnormal exit of the scoped section. The guard must
        // still restore Warn while unwinding.
        drop(inner);
        assert_eq!(log::max_level(), LevelFilter::Warn);
        panic!("simulated failure inside the scoped section");
    }));
    assert!(result.is_err(), "the panic must have been caught");

    // The guard was dropped before the panic and already restored Warn; the
    // outer level remains intact after catching.
    assert_eq!(log::max_level(), LevelFilter::Warn);
    assert_filtered_counts(logger, LevelFilter::Warn);

    // Also verify restoration when the guard itself is on the stack while
    // unwinding.
    log::set_max_level(LevelFilter::Info);
    let outer = log::set_max_level_scoped(LevelFilter::Debug);
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let _inner = log::set_max_level_scoped(LevelFilter::Off);
        assert_eq!(log::max_level(), LevelFilter::Off);
        panic!("simulated failure while guards are alive");
    }));
    assert!(result.is_err(), "the panic must have been caught");
    // The inner guard restored Debug during unwind; the outer guard is
    // still in scope.
    assert_eq!(log::max_level(), LevelFilter::Debug);
    drop(outer);
    assert_eq!(log::max_level(), LevelFilter::Info);

    log::set_max_level(LevelFilter::Trace);
}

#[test]
fn test_scoped_max_level_behaviors() {
    let logger = init();

    #[cfg(not(any(
        feature = "max_level_off",
        feature = "max_level_error",
        feature = "max_level_warn",
        feature = "max_level_info",
        feature = "max_level_debug",
        feature = "max_level_trace",
        feature = "release_max_level_off",
        feature = "release_max_level_error",
        feature = "release_max_level_warn",
        feature = "release_max_level_info",
        feature = "release_max_level_debug",
        feature = "release_max_level_trace",
    )))]
    {
        phase_nested_exit(logger);
        phase_explicit_update_wins(logger);
        phase_panic_restores(logger);

        // Registration state is unchanged after all three phases: a second
        // registration must fail, while records still reach the counting
        // logger. The per-phase accounting above already checked the record
        // counts against each active filter; summarize the running totals
        // for the --nocapture output.
        logger.reset();
        log::set_max_level(LevelFilter::Warn);
        assert_filtered_counts(logger, LevelFilter::Warn);
        assert_eq!(logger.counts().total(), 2);
        log::set_max_level(LevelFilter::Trace);

        println!(
            "scoped max-level acceptance: nested exit, explicit update wins, \
             panic restore all passed; counted totals after final phase = {:?}",
            logger.counts()
        );
    }

    // Level-state assertions that are valid regardless of static max level
    // features.
    log::set_max_level(LevelFilter::Info);
    let guard = log::set_max_level_scoped(LevelFilter::Error);
    assert_eq!(log::max_level(), LevelFilter::Error);
    drop(guard);
    assert_eq!(log::max_level(), LevelFilter::Info);

    // Logging still goes through the originally registered logger. A static
    // max level of `Off` may filter the record, so only check the logger is
    // still the registered one.
    let counts_before = logger.counts();
    error!("final registration check");
    let counts_after = logger.counts();
    if cfg!(not(any(
        feature = "max_level_off",
        feature = "release_max_level_off"
    ))) {
        assert_eq!(counts_after.error - counts_before.error, 1);
    }
    assert!(
        log::set_logger(&LOGGER).is_err(),
        "the logger registration must remain in place"
    );

    log::set_max_level(LevelFilter::Trace);
    assert_eq!(log::max_level(), LevelFilter::Trace);
}
