//! Targeted tests for the per-record strategy snapshot.
//!
//! These tests verify that:
//! - the level/target rule selected by `enabled` is fixed for the
//!   corresponding `log` call, so a rule update landing between the two cannot
//!   change the in-flight record's decision;
//! - the message is only evaluated when `enabled` allows it (a disabled
//!   message has a Display count of 0);
//! - each log agent is independent.

use log::{log, Level, Log, Metadata, Record};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A Display wrapper that counts how many times its `Display` impl is
/// evaluated.
struct CountingDisplay<'a> {
    count: &'a AtomicUsize,
}

impl fmt::Display for CountingDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.count.fetch_add(1, Ordering::SeqCst);
        write!(f, "counted")
    }
}

/// A log agent whose `enabled` flips its rule as a side effect of being
/// evaluated. This simulates a rule update landing between the `enabled`
/// decision and the corresponding `log` call of a single in-flight record.
struct FlipAgent {
    rule: AtomicBool,
    logged: AtomicUsize,
}

impl FlipAgent {
    fn new(rule: bool) -> Self {
        FlipAgent {
            rule: AtomicBool::new(rule),
            logged: AtomicUsize::new(0),
        }
    }
}

impl Log for FlipAgent {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        let decision = self.rule.load(Ordering::SeqCst);
        // A rule update lands right after the enabled decision is made.
        self.rule.store(!decision, Ordering::SeqCst);
        decision
    }

    fn log(&self, record: &Record) {
        self.logged.fetch_add(1, Ordering::SeqCst);
        // Formatting the record evaluates the message's Display impls.
        let _ = record.args().to_string();
    }

    fn flush(&self) {}
}

#[test]
fn snapshot_fixed_across_rule_flip() {
    log::set_max_level(log::LevelFilter::Trace);
    let agent = FlipAgent::new(true);
    let display_count = AtomicUsize::new(0);

    // Record 1: the rule starts enabled. `enabled` returns true and flips the
    // rule to disabled. The snapshot (true) is fixed, so the message is
    // evaluated and `log` is called.
    log!(logger: agent, Level::Info, "{}", CountingDisplay { count: &display_count });
    assert_eq!(agent.logged.load(Ordering::SeqCst), 1);
    assert_eq!(display_count.load(Ordering::SeqCst), 1);

    // Record 2: the rule is now disabled. `enabled` returns false and flips
    // the rule to enabled. The snapshot (false) is fixed, so the message is
    // not evaluated (Display count stays 1) and `log` is not called.
    log!(logger: agent, Level::Info, "{}", CountingDisplay { count: &display_count });
    assert_eq!(agent.logged.load(Ordering::SeqCst), 1);
    assert_eq!(display_count.load(Ordering::SeqCst), 1);

    // Record 3: the rule is now enabled again. `enabled` returns true and
    // flips the rule to disabled. The snapshot (true) is fixed, so the
    // message is evaluated and `log` is called.
    log!(logger: agent, Level::Info, "{}", CountingDisplay { count: &display_count });
    assert_eq!(agent.logged.load(Ordering::SeqCst), 2);
    assert_eq!(display_count.load(Ordering::SeqCst), 2);
}

#[test]
fn disabled_message_display_count_zero() {
    log::set_max_level(log::LevelFilter::Trace);
    // An agent whose rule is disabled from the start.
    let agent = FlipAgent::new(false);
    let display_count = AtomicUsize::new(0);

    log!(logger: agent, Level::Info, "{}", CountingDisplay { count: &display_count });

    // The message was never evaluated: Display count is 0 and `log` was not
    // called.
    assert_eq!(agent.logged.load(Ordering::SeqCst), 0);
    assert_eq!(display_count.load(Ordering::SeqCst), 0);
}

#[test]
fn two_agents_independent() {
    log::set_max_level(log::LevelFilter::Trace);
    let agent_a = FlipAgent::new(true);
    let agent_b = FlipAgent::new(false);
    let count_a = AtomicUsize::new(0);
    let count_b = AtomicUsize::new(0);

    // Agent A starts enabled; agent B starts disabled.
    log!(logger: agent_a, Level::Info, "{}", CountingDisplay { count: &count_a });
    log!(logger: agent_b, Level::Info, "{}", CountingDisplay { count: &count_b });

    assert_eq!(agent_a.logged.load(Ordering::SeqCst), 1);
    assert_eq!(agent_b.logged.load(Ordering::SeqCst), 0);
    assert_eq!(count_a.load(Ordering::SeqCst), 1);
    assert_eq!(count_b.load(Ordering::SeqCst), 0);

    // After the flip, agent A is disabled and agent B is enabled.
    log!(logger: agent_a, Level::Info, "{}", CountingDisplay { count: &count_a });
    log!(logger: agent_b, Level::Info, "{}", CountingDisplay { count: &count_b });

    assert_eq!(agent_a.logged.load(Ordering::SeqCst), 1);
    assert_eq!(agent_b.logged.load(Ordering::SeqCst), 1);
    assert_eq!(count_a.load(Ordering::SeqCst), 1);
    assert_eq!(count_b.load(Ordering::SeqCst), 1);
}
