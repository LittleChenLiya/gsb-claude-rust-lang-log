//! Targeted tests for a per-record policy snapshot on a logging proxy.
//!
//! The [`LogProxy`] fixture is a small, per-agent logging facade that keeps its
//! own mutable filter policy (a selected level *and* a per-target rule). The
//! global logger registration in `log` is left untouched: the proxy is always
//! passed explicitly through the `logger:` argument of the logging macros.
//!
//! Semantics under test:
//!
//! * `Log::enabled` takes a *snapshot* of the level/target decision for the
//!   single upcoming record. Updating the policy between `enabled` and `log`
//!   (i.e. while a record is in flight) cannot change that record's decision.
//! * The message (`fmt::Arguments`, so every `Display` implementation inside
//!   it) is only evaluated for records the snapshot allows.
//! * Snapshots/pending decisions are kept independently per proxy instance, so
//!   two agents never influence each other.
//!
//! No rule-conflict compilation or other machinery is introduced: `enabled`
//! simply evaluates the current level + target rule once and freezes the
//! resulting verdict.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The target whose rule the fixture understands.
const EVENT_TARGET: &str = "events";

/// What the configured target is allowed to do, independent of the level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum TargetRule {
    /// Records targeting the watched target are allowed (subject to level).
    Allow = 0,
    /// Records targeting the watched target are always denied.
    Deny = 1,
}

/// An immutable copy of the policy bits that mattered for one record.
///
/// This is the "single-record policy snapshot": the selected level and the
/// target rule are frozen at `enabled` time.
#[derive(Clone, Copy, Debug)]
struct FrozenRules {
    level: LevelFilter,
    target: TargetRule,
}

impl FrozenRules {
    /// Apply the frozen rules to a record with the given level and target.
    fn allows(&self, level: Level, target: &str) -> bool {
        level <= self.level && target == EVENT_TARGET && self.target == TargetRule::Allow
    }
}

/// One parked in-flight decision: a snapshot owned by a specific proxy.
///
/// The log macros invoke `enabled` and then `log` as two separate trait method
/// calls with nothing shared between them, so the frozen decision is stashed
/// here keyed by the proxy's unique id.
#[derive(Clone, Copy)]
struct InFlight {
    proxy_id: usize,
    rules: FrozenRules,
}

thread_local! {
    static IN_FLIGHT: RefCell<Vec<InFlight>> = const { RefCell::new(Vec::new()) };
}

/// Shared, mutable policy of one logging agent.
struct ProxyState {
    level: AtomicUsize,
    target: AtomicUsize,
    records: Mutex<Vec<(Level, String)>>,
}

/// A logging proxy for a single agent. Each instance is independent and does
/// not touch the global `log` registration.
struct LogProxy {
    id: usize,
    state: ProxyState,
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

impl LogProxy {
    fn new(level: LevelFilter, target: TargetRule) -> LogProxy {
        LogProxy {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            state: ProxyState {
                level: AtomicUsize::new(level as usize),
                target: AtomicUsize::new(target as usize),
                records: Mutex::new(Vec::new()),
            },
        }
    }

    /// Replace the level part of the policy. In-flight decisions are unaffected.
    fn set_level(&self, level: LevelFilter) {
        self.state.level.store(level as usize, Ordering::Relaxed);
    }

    /// Replace the target part of the policy. In-flight decisions are unaffected.
    fn set_target_rule(&self, rule: TargetRule) {
        self.state.target.store(rule as usize, Ordering::Relaxed);
    }

    fn current_rules(&self) -> FrozenRules {
        FrozenRules {
            level: unsafe {
                std::mem::transmute::<usize, LevelFilter>(self.state.level.load(Ordering::Relaxed))
            },
            target: unsafe {
                std::mem::transmute::<usize, TargetRule>(self.state.target.load(Ordering::Relaxed))
            },
        }
    }

    /// Snapshot the current decision for one record and park it for the
    /// upcoming `log` call.
    fn park_snapshot(&self, rules: FrozenRules) {
        IN_FLIGHT.with(|in_flight| {
            in_flight.borrow_mut().push(InFlight {
                proxy_id: self.id,
                rules,
            });
        });
    }

    /// Take the snapshot previously taken by *this* proxy. Falls back to the
    /// live policy if `log` is reached without a preceding `enabled` (the
    /// macros always pair the two; this only covers direct trait calls).
    fn take_snapshot(&self) -> FrozenRules {
        IN_FLIGHT.with(|in_flight| {
            let mut slots = in_flight.borrow_mut();
            match slots.iter().position(|slot| slot.proxy_id == self.id) {
                Some(pos) => slots.remove(pos).rules,
                None => self.current_rules(),
            }
        })
    }

    fn records(&self) -> Vec<(Level, String)> {
        self.state.records.lock().unwrap().clone()
    }
}

impl Log for LogProxy {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // Freeze the level/target decision for exactly the next record.
        let rules = self.current_rules();
        let verdict = rules.allows(metadata.level(), metadata.target());
        self.park_snapshot(rules);
        verdict
    }

    fn log(&self, record: &Record) {
        // Consume the snapshot taken by `enabled`. Any policy update that
        // happened in between is irrelevant.
        let rules = self.take_snapshot();
        if !rules.allows(record.level(), record.target()) {
            return;
        }

        // `record.args()` is only touched when the snapshot allows the record,
        // so `Display` code inside the message is never evaluated otherwise.
        self.state
            .records
            .lock()
            .unwrap()
            .push((record.level(), record.args().to_string()));
    }

    fn flush(&self) {}
}

/// A value whose `Display` implementation counts how often it ran.
struct CountingDisplay<'a> {
    count: &'a AtomicUsize,
    label: &'static str,
}

impl fmt::Display for CountingDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.count.fetch_add(1, Ordering::Relaxed);
        f.write_str(self.label)
    }
}

fn allow_trace() {
    // Keep the facade-wide gate fully open so the proxy's own snapshot is the
    // only thing deciding whether a record is emitted.
    log::set_max_level(LevelFilter::Trace);
}

/// Rules changed between `enabled` and `log` still follow the old snapshot, for
/// both the level and the target rule.
#[test]
fn in_flight_record_uses_frozen_snapshot() {
    allow_trace();

    let proxy = LogProxy::new(LevelFilter::Warn, TargetRule::Allow);

    // Record 1: the level is Warn when `enabled` runs, so Info is parked as a
    // deny. Raise the level to Trace while the record is in flight; the frozen
    // snapshot still denies it.
    let _ = log::log_enabled!(logger: proxy, target: "events", Level::Info);
    proxy.set_level(LevelFilter::Trace);
    log::info!(logger: proxy, target: "events", "level should stay frozen");

    // Record 2: the target rule is Allow when `enabled` runs. Flip it to Deny
    // while the record is in flight; the frozen Allow verdict still emits it.
    let _ = log::log_enabled!(logger: proxy, target: "events", Level::Info);
    proxy.set_target_rule(TargetRule::Deny);
    log::info!(logger: proxy, target: "events", "target should stay frozen");

    // Record 3: the snapshot is single-use. With the target rule now Deny, a
    // fresh record uses the new policy and is denied.
    log::info!(logger: proxy, target: "events", "fresh deny must apply");

    assert_eq!(
        proxy.records(),
        vec![(Level::Info, "target should stay frozen".to_string())]
    );
}

/// A denied message never has its `Display` arguments evaluated, even if the
/// policy is loosened before `log`; an allowed message is evaluated exactly
/// once.
#[test]
fn disabled_message_display_is_never_evaluated() {
    allow_trace();

    let proxy = LogProxy::new(LevelFilter::Off, TargetRule::Allow);
    let count = AtomicUsize::new(0);

    // Denied by the frozen Off snapshot; loosening the level in between cannot
    // resurrect the record, so the message is never formatted.
    let _ = log::log_enabled!(logger: proxy, target: "events", Level::Info);
    proxy.set_level(LevelFilter::Trace);
    log::info!(
        logger: proxy,
        target: "events",
        "denied {}",
        CountingDisplay {
            count: &count,
            label: "nope",
        }
    );
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(proxy.records().is_empty());

    // A genuinely allowed record formats exactly once.
    log::info!(
        logger: proxy,
        target: "events",
        "allowed {}",
        CountingDisplay {
            count: &count,
            label: "yes",
        }
    );
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert_eq!(
        proxy.records(),
        vec![(Level::Info, "allowed yes".to_string())]
    );
}

/// Two proxies keep independent policies and independent in-flight snapshots.
#[test]
fn two_proxies_do_not_affect_each_other() {
    allow_trace();

    // `agent_one` starts fully open and freezes an Allow decision; `agent_two`
    // starts fully closed and freezes a Deny decision.
    let agent_one = LogProxy::new(LevelFilter::Trace, TargetRule::Allow);
    let agent_two = LogProxy::new(LevelFilter::Off, TargetRule::Deny);

    // Each proxy takes its own snapshot...
    let _ = log::log_enabled!(logger: agent_one, target: "events", Level::Info);
    let _ = log::log_enabled!(logger: agent_two, target: "events", Level::Info);

    // ...then the two agents swap policies. Neither frozen decision moves.
    agent_one.set_level(LevelFilter::Off);
    agent_one.set_target_rule(TargetRule::Deny);
    agent_two.set_level(LevelFilter::Trace);
    agent_two.set_target_rule(TargetRule::Allow);

    log::info!(logger: agent_one, target: "events", "agent one frozen allow");
    log::info!(logger: agent_two, target: "events", "agent two frozen deny");

    // Subsequent records use each proxy's independent live policy.
    log::info!(logger: agent_one, target: "events", "agent one now closed");
    log::info!(logger: agent_two, target: "events", "agent two now open");

    assert_eq!(
        agent_one.records(),
        vec![(Level::Info, "agent one frozen allow".to_string())]
    );
    assert_eq!(
        agent_two.records(),
        vec![(Level::Info, "agent two now open".to_string())]
    );
}
