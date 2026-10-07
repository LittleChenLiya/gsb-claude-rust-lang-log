//! Tests for `set_max_level_scoped` / `ScopedMaxLevelGuard`.
//!
//! Everything runs in a single test function so the global maximum level is
//! only ever mutated from one thread, keeping the scenario deterministic.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::atomic::{AtomicUsize, Ordering};

const ZERO: AtomicUsize = AtomicUsize::new(0);
/// Number of records received per level, indexed by `Level as usize`
/// (slot 0 is unused, `Off` is never logged).
static COUNTS: [AtomicUsize; 6] = [ZERO, ZERO, ZERO, ZERO, ZERO, ZERO];

struct CountingLogger;

impl Log for CountingLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        COUNTS[record.level() as usize].fetch_add(1, Ordering::SeqCst);
    }

    fn flush(&self) {}
}

static LOGGER: CountingLogger = CountingLogger;

fn count(level: Level) -> usize {
    COUNTS[level as usize].load(Ordering::SeqCst)
}

fn total() -> usize {
    COUNTS.iter().map(|c| c.load(Ordering::SeqCst)).sum()
}

/// Emit one record at each of the five levels.
fn emit_all() {
    log::error!("error");
    log::warn!("warn");
    log::info!("info");
    log::debug!("debug");
    log::trace!("trace");
}

#[test]
fn scoped_max_level_guard() {
    // These tests don't make sense when a static max level filter is
    // compiled in, exactly like the existing integration tests.
    #[cfg(any(
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
    ))]
    return;

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
        log::set_logger(&LOGGER).expect("logger registration failed");
        let registered = log::logger() as *const dyn Log;

        // 1. Nested guards restore in stack (LIFO) order.
        log::set_max_level(LevelFilter::Info);
        emit_all(); // error, warn, info -> 3 records

        let outer = log::set_max_level_scoped(LevelFilter::Warn);
        assert_eq!(outer.previous_level(), LevelFilter::Info);
        assert_eq!(outer.level(), LevelFilter::Warn);
        assert_eq!(log::max_level(), LevelFilter::Warn);
        emit_all(); // error, warn -> 2 records

        let inner = log::set_max_level_scoped(LevelFilter::Error);
        assert_eq!(inner.previous_level(), LevelFilter::Warn);
        assert_eq!(log::max_level(), LevelFilter::Error);
        emit_all(); // error -> 1 record

        drop(inner);
        assert_eq!(log::max_level(), LevelFilter::Warn);
        emit_all(); // error, warn -> 2 records

        drop(outer);
        assert_eq!(log::max_level(), LevelFilter::Info);

        // 2. An explicit `set_max_level` is not overwritten by a stale guard.
        let stale = log::set_max_level_scoped(LevelFilter::Trace);
        assert_eq!(log::max_level(), LevelFilter::Trace);
        log::set_max_level(LevelFilter::Error);
        emit_all(); // error -> 1 record
        drop(stale);
        assert_eq!(log::max_level(), LevelFilter::Error);

        // 3. A guard still restores its level when the scope unwinds.
        log::set_max_level(LevelFilter::Debug);
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the test output quiet
        let panicked = std::panic::catch_unwind(|| {
            let _guard = log::set_max_level_scoped(LevelFilter::Error);
            assert_eq!(log::max_level(), LevelFilter::Error);
            emit_all(); // error -> 1 record
            panic!("simulated failure inside a scoped level");
        });
        let _ = std::panic::take_hook();
        std::panic::set_hook(prev_hook);
        assert!(panicked.is_err());
        assert_eq!(log::max_level(), LevelFilter::Debug);
        emit_all(); // error, warn, info, debug -> 4 records

        // The registered logger is untouched by all of the above.
        assert!(std::ptr::eq(registered, log::logger() as *const dyn Log));
        assert!(log::set_logger(&LOGGER).is_err());

        // Record counts: error fired in all 7 emit_all calls; warn only when
        // the max level allowed it (Info/Warn/Warn/Debug), info twice
        // (Info/Debug), debug once (Debug), trace never.
        assert_eq!(count(Level::Error), 7);
        assert_eq!(count(Level::Warn), 4);
        assert_eq!(count(Level::Info), 2);
        assert_eq!(count(Level::Debug), 1);
        assert_eq!(count(Level::Trace), 0);
        assert_eq!(total(), 14);
    }
}
