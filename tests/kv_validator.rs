//! Acceptance tests for the explicit KV source validator.
//!
//! Run locally:
//!
//! ```sh
//! cargo test --features kv_std --test kv_validator
//! cargo test --features kv_serde --test kv_validator
//! ```

#![cfg(all(feature = "kv", feature = "alloc"))]

use log::kv::{AllowedKeys, DuplicatePolicy, Source, Validator, ValueKind, VisitSource};
use log::kv::{Error, Key, ToValue, Value};
use log::Record;
use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

fn allow_all() -> AllowedKeys {
    let mut allowed = AllowedKeys::new();
    allowed
        .allow("a", &[ValueKind::U64, ValueKind::I64, ValueKind::Str])
        .allow("b", &[ValueKind::U64, ValueKind::I64, ValueKind::Str])
        .allow("c", &[ValueKind::U64, ValueKind::I64, ValueKind::Str])
        .allow("opaque", &[ValueKind::Opaque]);
    allowed
}

#[test]
fn empty_source_is_valid() {
    let source: Option<(&str, u64)> = None;

    let report = Validator::new(&allow_all()).validate(&source);

    assert!(report.complete);
    assert!(report.pairs.is_empty());
    assert!(report.error.is_none());
}

#[test]
fn source_is_visited_exactly_once() {
    struct CountingSource {
        visits: Cell<usize>,
    }

    impl Source for CountingSource {
        fn visit<'kvs>(&'kvs self, visitor: &mut dyn VisitSource<'kvs>) -> Result<(), Error> {
            self.visits.set(self.visits.get() + 1);
            visitor.visit_pair(Key::from_str("a"), 1u64.to_value())
        }
    }

    let source = CountingSource {
        visits: Cell::new(0),
    };

    let report = Validator::new(&allow_all()).validate(&source);

    assert!(report.complete);
    assert_eq!(1, source.visits.get());
}

#[test]
fn two_same_named_keys_report_duplicates_per_policy() {
    let source = [("a", 1u64), ("a", 2u64)];

    // Warn: both occurrences kept, duplicate reported, visit completes.
    let report = Validator::new(&allow_all())
        .duplicates(DuplicatePolicy::Warn)
        .validate(&source);
    assert!(report.complete);
    assert_eq!(2, report.pairs.len());
    assert_eq!(1, report.duplicates.len());
    assert_eq!("a", report.duplicates[0].key);
    assert_eq!(0, report.duplicates[0].first);
    assert_eq!(1, report.duplicates[0].second);

    // Deny: visit stops at the second occurrence, prefix reported.
    let report = Validator::new(&allow_all())
        .duplicates(DuplicatePolicy::Deny)
        .validate(&source);
    assert!(!report.complete);
    assert_eq!(1, report.pairs.len());
    assert!(matches!(
        report.error,
        Some(log::kv::ValidationError::DuplicateKey(ref key)) if key == "a"
    ));
}

#[test]
fn opaque_value_display_count_is_zero() {
    struct CountingDisplay {
        count: Rc<Cell<usize>>,
    }

    impl fmt::Display for CountingDisplay {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            self.count.set(self.count.get() + 1);
            f.write_str("opaque")
        }
    }

    let count = Rc::new(Cell::new(0));
    let display = CountingDisplay {
        count: count.clone(),
    };
    let value = Value::from_display(&display);
    let source = [("opaque", value)];

    let report = Validator::new(&allow_all()).validate(&source);

    assert!(report.complete);
    assert_eq!(ValueKind::Opaque, report.pairs[0].kind);
    assert_eq!(
        0,
        count.get(),
        "opaque value must never be forced through Display"
    );
}

#[test]
fn third_visitor_failure_leaves_two_incomplete_receipts() {
    let source = [("a", 1u64), ("b", 2u64), ("c", 3u64)];
    let mut allowed = AllowedKeys::new();
    allowed
        .allow("a", &[ValueKind::U64])
        .allow("b", &[ValueKind::U64]);

    let report = Validator::new(&allowed).validate(&source);

    assert!(!report.complete);
    assert_eq!(2, report.pairs.len());
    assert_eq!("a", report.pairs[0].key);
    assert_eq!("b", report.pairs[1].key);
    assert!(matches!(
        report.error,
        Some(log::kv::ValidationError::UnknownKey(ref key)) if key == "c"
    ));
}

#[test]
fn four_field_record_example() {
    let source = [
        ("request_id", Value::from("req-8a2f")),
        ("method", Value::from("GET")),
        ("status", Value::from(200u64)),
        ("latency_ms", Value::from(12u64)),
    ];

    let record = Record::builder()
        .args(format_args!("handled request"))
        .key_values(&source)
        .build();

    let mut allowed = AllowedKeys::new();
    allowed
        .allow("request_id", &[ValueKind::Str])
        .allow("method", &[ValueKind::Str])
        .allow("status", &[ValueKind::U64, ValueKind::I64])
        .allow("latency_ms", &[ValueKind::U64, ValueKind::I64]);

    let report = Validator::new(&allowed)
        .duplicates(DuplicatePolicy::Deny)
        .validate(record.key_values());

    assert!(report.complete);
    assert_eq!(4, report.pairs.len());
    assert_eq!(
        vec!["request_id", "method", "status", "latency_ms"],
        report
            .pairs
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>()
    );
}
