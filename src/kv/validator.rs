//! An explicit validator for key-value [`Source`]s.
//!
//! This module provides [`Validator`], a small checker that inspects a source
//! through the standard [`VisitSource`] / [`VisitValue`] APIs only. It is
//! *explicit* in the sense that it never parses fields out of a `Debug` string
//! and never forces an unknown opaque value through `Display`:
//!
//! - **Access order and duplicates are preserved.** The validator walks the
//!   source in order and records every pair it is given, including repeated
//!   keys, instead of collapsing them into a map.
//! - **The source is visited exactly once.** [`Validator::validate`] calls
//!   [`Source::visit`] a single time and never follows up with `get` or
//!   `count`, so one-shot sources are not consumed twice.
//! - **Only the category of each value is kept.** The report stores the key
//!   and the [`ValueKind`] of each value; the value itself is discarded.
//! - **Visitor errors stop the visit, but the already visited prefix is
//!   reported.** If the source (or a policy violation) returns an error, the
//!   pairs visited before the failure are still in the report, and the report
//!   is marked as incomplete.
//!
//! The default [`log::Record`](crate::Record) is left untouched: the validator
//! works on any `&dyn Source`, including
//! [`Record::key_values`](crate::Record::key_values).
//!
//! # Local cargo commands
//!
//! Run the four-field example:
//!
//! ```sh
//! cargo run --example kv_validate --features kv_std
//! ```
//!
//! Run the validator tests locally:
//!
//! ```sh
//! cargo test --features kv_std --test kv_validator
//! cargo test --features kv_serde --test kv_validator
//! ```
//!
//! # Example
//!
//! ```
//! use log::kv::{AllowedKeys, DuplicatePolicy, Validator, Value, ValueKind};
//!
//! let source = [
//!     ("request_id", Value::from("req-8a2f")),
//!     ("method", Value::from("GET")),
//!     ("status", Value::from(200u64)),
//!     ("latency_ms", Value::from(12u64)),
//! ];
//!
//! let mut allowed = AllowedKeys::new();
//! allowed
//!     .allow("request_id", &[ValueKind::Str])
//!     .allow("method", &[ValueKind::Str])
//!     .allow("status", &[ValueKind::U64, ValueKind::I64])
//!     .allow("latency_ms", &[ValueKind::U64, ValueKind::I64]);
//!
//! let report = Validator::new(&allowed)
//!     .duplicates(DuplicatePolicy::Deny)
//!     .validate(&source);
//!
//! assert!(report.complete);
//! assert_eq!(4, report.pairs.len());
//! ```

use crate::kv::{Error, Key, Source, Value, VisitSource, VisitValue};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// The category of a value, without the value itself.
///
/// This is the value's *kind* (null, boolean, integer, float, string, ...),
/// not the value. It is obtained through the [`VisitValue`] API, so an
/// [`Opaque`](ValueKind::Opaque) value is never formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueKind {
    /// An empty value (`null`).
    Null,
    /// A boolean.
    Bool,
    /// An unsigned 64-bit integer.
    U64,
    /// A signed 64-bit integer.
    I64,
    /// An unsigned 128-bit integer.
    U128,
    /// A signed 128-bit integer.
    I128,
    /// A floating point number.
    F64,
    /// A string.
    Str,
    /// A Unicode character.
    Char,
    /// A `std::error::Error` value (only produced with the `kv_std` feature).
    Error,
    /// A value captured through `Debug`, `Display`, `serde` or `sval` whose
    /// concrete kind is unknown. The validator does **not** format it.
    Opaque,
}

/// A table describing which [`ValueKind`]s are allowed for each key.
///
/// Keys not present in the table are rejected as unknown.
#[derive(Debug, Clone, Default)]
pub struct AllowedKeys {
    table: BTreeMap<String, Vec<ValueKind>>,
}

impl AllowedKeys {
    /// Create an empty table.
    pub fn new() -> Self {
        AllowedKeys::default()
    }

    /// Allow the given value kinds for `key`.
    pub fn allow(&mut self, key: impl Into<String>, kinds: &[ValueKind]) -> &mut Self {
        self.table.insert(key.into(), kinds.to_vec());
        self
    }

    fn get(&self, key: &str) -> Option<&[ValueKind]> {
        self.table.get(key).map(|kinds| kinds.as_slice())
    }
}

/// How duplicate keys are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DuplicatePolicy {
    /// Record duplicates but keep visiting.
    ///
    /// Both occurrences appear in the report and a [`Duplicate`] finding is
    /// recorded, but validation continues and the report can still be
    /// [`complete`](Report::complete).
    Warn,
    /// Treat a duplicate key as a hard error.
    ///
    /// The visit stops at the second occurrence; the pairs visited before it
    /// remain in the report and the report is incomplete.
    Deny,
}

/// A visited key-value pair.
///
/// Only the key and the value's [`ValueKind`] are kept — never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairReport {
    /// The key, in the source's own access order.
    pub key: String,
    /// The category of the value.
    pub kind: ValueKind,
}

/// A duplicate-key finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    /// The duplicated key.
    pub key: String,
    /// The index of the first occurrence (into [`Report::pairs`]).
    pub first: usize,
    /// The index of the duplicate occurrence.
    pub second: usize,
}

/// A policy violation that stopped validation.
#[derive(Debug)]
#[non_exhaustive]
pub enum ValidationError {
    /// A key is not present in the [`AllowedKeys`] table.
    UnknownKey(String),
    /// A value's [`ValueKind`] is not allowed for its key.
    DisallowedKind {
        /// The offending key.
        key: String,
        /// The offending value's category.
        kind: ValueKind,
    },
    /// A duplicate key was found while the policy is [`DuplicatePolicy::Deny`].
    DuplicateKey(String),
    /// The source itself returned an error while visiting.
    Source(Error),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ValidationError::UnknownKey(key) => write!(f, "unknown key: {key:?}"),
            ValidationError::DisallowedKind { key, kind } => {
                write!(f, "value of kind {kind:?} is not allowed for key {key:?}")
            }
            ValidationError::DuplicateKey(key) => write!(f, "duplicate key: {key:?}"),
            ValidationError::Source(err) => write!(f, "source error: {err}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ValidationError::Source(err) => Some(err),
            _ => None,
        }
    }
}

/// The result of validating a source.
///
/// The report only contains categories and keys — no values.
#[derive(Debug)]
pub struct Report {
    /// Pairs visited before validation stopped, in access order.
    ///
    /// Duplicate keys are kept (unless the policy is [`DuplicatePolicy::Deny`],
    /// in which case the duplicate occurrence stops the visit and is not
    /// recorded here).
    pub pairs: Vec<PairReport>,
    /// Duplicate-key findings, in order of occurrence.
    pub duplicates: Vec<Duplicate>,
    /// Whether the source was fully visited.
    ///
    /// `false` means the source (or a policy violation) returned an error
    /// before every pair was visited; the report then only covers the visited
    /// prefix.
    pub complete: bool,
    /// The error that stopped validation, if any.
    pub error: Option<ValidationError>,
}

impl Report {
    /// Returns `true` if the source was fully visited without errors.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Iterate over the visited pairs in access order.
    pub fn pairs(&self) -> impl Iterator<Item = &PairReport> {
        self.pairs.iter()
    }
}

/// An explicit, one-shot validator for [`Source`]s.
///
/// See the [module documentation](self) for the full design.
#[derive(Debug)]
pub struct Validator<'a> {
    allowed: &'a AllowedKeys,
    duplicates: DuplicatePolicy,
}

impl<'a> Validator<'a> {
    /// Create a validator against the given allowed-key table.
    ///
    /// The default duplicate policy is [`DuplicatePolicy::Warn`].
    pub fn new(allowed: &'a AllowedKeys) -> Self {
        Validator {
            allowed,
            duplicates: DuplicatePolicy::Warn,
        }
    }

    /// Set the duplicate-key policy.
    pub fn duplicates(mut self, policy: DuplicatePolicy) -> Self {
        self.duplicates = policy;
        self
    }

    /// Validate `source`, visiting it exactly once.
    ///
    /// The source is visited through [`Source::visit`] a single time; no
    /// follow-up `get` or `count` calls are made. If the visit returns an
    /// error, the already visited prefix is reported and the report is marked
    /// incomplete.
    pub fn validate<'kvs>(self, source: &'kvs dyn Source) -> Report {
        let mut visitor = ValidationVisitor {
            allowed: self.allowed,
            duplicates: self.duplicates,
            pairs: Vec::new(),
            seen: BTreeMap::new(),
            duplicate_findings: Vec::new(),
            error: None,
            stopped: false,
        };

        let result = source.visit(&mut visitor);

        if let Err(source_error) = result {
            // If the visitor already recorded a policy violation, keep it;
            // otherwise the source itself is the one that failed.
            if visitor.error.is_none() {
                visitor.error = Some(ValidationError::Source(source_error));
            }
        }

        let complete = visitor.error.is_none();

        Report {
            pairs: visitor.pairs,
            duplicates: visitor.duplicate_findings,
            complete,
            error: visitor.error,
        }
    }
}

/// The [`VisitSource`] that performs the validation.
///
/// Pairs are only recorded once every check has passed, so the report always
/// holds the successfully visited prefix.
struct ValidationVisitor<'a> {
    allowed: &'a AllowedKeys,
    duplicates: DuplicatePolicy,
    pairs: Vec<PairReport>,
    seen: BTreeMap<String, usize>,
    duplicate_findings: Vec<Duplicate>,
    error: Option<ValidationError>,
    stopped: bool,
}

impl<'kvs> VisitSource<'kvs> for ValidationVisitor<'_> {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), Error> {
        // A well-behaved source stops calling us after we return an error.
        // If a misbehaving one calls us again, do not record anything more.
        if self.stopped {
            return Ok(());
        }

        let key = key.as_str();
        let kind = classify(&value);
        let index = self.pairs.len();

        // Duplicate detection: report, and possibly stop.
        if let Some(&first) = self.seen.get(key) {
            self.duplicate_findings.push(Duplicate {
                key: String::from(key),
                first,
                second: index,
            });

            if self.duplicates == DuplicatePolicy::Deny {
                self.stopped = true;
                self.error = Some(ValidationError::DuplicateKey(String::from(key)));
                return Err(Error::msg("duplicate key"));
            }
        }

        // Allowed-key table: reject unknown keys.
        let Some(kinds) = self.allowed.get(key) else {
            self.stopped = true;
            self.error = Some(ValidationError::UnknownKey(String::from(key)));
            return Err(Error::msg("unknown key"));
        };

        // Value kind: reject disallowed categories.
        if !kinds.contains(&kind) {
            self.stopped = true;
            self.error = Some(ValidationError::DisallowedKind {
                key: String::from(key),
                kind,
            });
            return Err(Error::msg("value kind is not allowed for key"));
        }

        // All checks passed: record the receipt.
        if !self.seen.contains_key(key) {
            self.seen.insert(String::from(key), index);
        }
        self.pairs.push(PairReport {
            key: String::from(key),
            kind,
        });

        Ok(())
    }
}

/// Classify a value by kind without ever formatting it.
///
/// The [`VisitValue`] API tells us the concrete category of primitive values;
/// anything that falls through to [`VisitValue::visit_any`] is opaque and is
/// reported as [`ValueKind::Opaque`] — its `Debug`/`Display` implementation is
/// never invoked.
fn classify(value: &Value<'_>) -> ValueKind {
    struct Classify(Option<ValueKind>);

    impl<'v> VisitValue<'v> for Classify {
        fn visit_any(&mut self, _value: Value) -> Result<(), Error> {
            // Opaque: captured via Debug/Display/serde/sval. Do not format.
            self.0 = Some(ValueKind::Opaque);
            Ok(())
        }

        fn visit_null(&mut self) -> Result<(), Error> {
            self.0 = Some(ValueKind::Null);
            Ok(())
        }

        fn visit_bool(&mut self, _value: bool) -> Result<(), Error> {
            self.0 = Some(ValueKind::Bool);
            Ok(())
        }

        fn visit_u64(&mut self, _value: u64) -> Result<(), Error> {
            self.0 = Some(ValueKind::U64);
            Ok(())
        }

        fn visit_i64(&mut self, _value: i64) -> Result<(), Error> {
            self.0 = Some(ValueKind::I64);
            Ok(())
        }

        fn visit_u128(&mut self, _value: u128) -> Result<(), Error> {
            self.0 = Some(ValueKind::U128);
            Ok(())
        }

        fn visit_i128(&mut self, _value: i128) -> Result<(), Error> {
            self.0 = Some(ValueKind::I128);
            Ok(())
        }

        fn visit_f64(&mut self, _value: f64) -> Result<(), Error> {
            self.0 = Some(ValueKind::F64);
            Ok(())
        }

        fn visit_str(&mut self, _value: &str) -> Result<(), Error> {
            self.0 = Some(ValueKind::Str);
            Ok(())
        }

        fn visit_borrowed_str(&mut self, _value: &'v str) -> Result<(), Error> {
            self.0 = Some(ValueKind::Str);
            Ok(())
        }

        fn visit_char(&mut self, _value: char) -> Result<(), Error> {
            self.0 = Some(ValueKind::Char);
            Ok(())
        }

        #[cfg(feature = "kv_std")]
        fn visit_error(&mut self, _err: &(dyn std::error::Error + 'static)) -> Result<(), Error> {
            self.0 = Some(ValueKind::Error);
            Ok(())
        }

        #[cfg(feature = "kv_std")]
        fn visit_borrowed_error(
            &mut self,
            _err: &'v (dyn std::error::Error + 'static),
        ) -> Result<(), Error> {
            self.0 = Some(ValueKind::Error);
            Ok(())
        }
    }

    let mut classifier = Classify(None);

    // Classification is infallible for the categories we care about. If the
    // underlying value fails to visit, treat it as opaque rather than forcing
    // it through Display.
    let _ = value.visit(&mut classifier);

    classifier.0.unwrap_or(ValueKind::Opaque)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::{Key, ToValue};
    use std::cell::Cell;
    use std::fmt;
    use std::rc::Rc;

    fn allow_all() -> AllowedKeys {
        let mut allowed = AllowedKeys::new();
        allowed
            .allow("a", &[ValueKind::U64, ValueKind::I64, ValueKind::Str])
            .allow("b", &[ValueKind::U64, ValueKind::I64, ValueKind::Str])
            .allow("c", &[ValueKind::U64, ValueKind::I64, ValueKind::Str])
            .allow("opaque", &[ValueKind::Opaque])
            .allow("dbg", &[ValueKind::Opaque]);
        allowed
    }

    #[test]
    fn empty_source_is_valid() {
        let source: Option<(&str, u64)> = None;

        let report = Validator::new(&allow_all()).validate(&source);

        assert!(report.is_complete());
        assert!(report.complete);
        assert!(report.pairs.is_empty());
        assert!(report.duplicates.is_empty());
        assert!(report.error.is_none());
    }

    #[test]
    fn visits_source_exactly_once() {
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
        assert_eq!(1, report.pairs.len());
    }

    #[test]
    fn preserves_access_order() {
        let source = [("a", 1u64), ("b", 2u64), ("c", 3u64)];

        let report = Validator::new(&allow_all()).validate(&source);

        assert!(report.complete);
        let keys: Vec<&str> = report.pairs.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(vec!["a", "b", "c"], keys);
        assert_eq!(ValueKind::U64, report.pairs[0].kind);
    }

    #[test]
    fn duplicates_reported_per_policy() {
        let source = [("a", 1u64), ("a", 2u64)];

        let report = Validator::new(&allow_all())
            .duplicates(DuplicatePolicy::Warn)
            .validate(&source);

        // Both occurrences are kept in order.
        assert!(report.complete);
        assert_eq!(2, report.pairs.len());
        assert_eq!("a", report.pairs[0].key);
        assert_eq!("a", report.pairs[1].key);

        // The duplicate is reported.
        assert_eq!(1, report.duplicates.len());
        assert_eq!("a", report.duplicates[0].key);
        assert_eq!(0, report.duplicates[0].first);
        assert_eq!(1, report.duplicates[0].second);
    }

    #[test]
    fn duplicate_deny_stops_and_reports_prefix() {
        let source = [("a", 1u64), ("a", 2u64)];

        let report = Validator::new(&allow_all())
            .duplicates(DuplicatePolicy::Deny)
            .validate(&source);

        // The visit stops at the second `a`; only the first receipt is kept.
        assert!(!report.complete);
        assert_eq!(1, report.pairs.len());
        assert_eq!("a", report.pairs[0].key);
        assert_eq!(1, report.duplicates.len());
        assert!(matches!(
            report.error,
            Some(ValidationError::DuplicateKey(ref key)) if key == "a"
        ));
    }

    #[test]
    fn third_visitor_failure_leaves_two_incomplete_receipts() {
        // The third pair is rejected as an unknown key.
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
            Some(ValidationError::UnknownKey(ref key)) if key == "c"
        ));
    }

    #[test]
    fn disallowed_kind_stops_and_reports_prefix() {
        let source = [("a", 1u64), ("b", 2u64), ("c", 3u64)];
        let mut allowed = AllowedKeys::new();
        allowed
            .allow("a", &[ValueKind::U64])
            .allow("b", &[ValueKind::U64])
            .allow("c", &[ValueKind::Str]); // u64 is not allowed for `c`

        let report = Validator::new(&allowed).validate(&source);

        assert!(!report.complete);
        assert_eq!(2, report.pairs.len());
        assert!(matches!(
            report.error,
            Some(ValidationError::DisallowedKind {
                ref key,
                kind: ValueKind::U64,
            }) if key == "c"
        ));
    }

    #[test]
    fn opaque_value_is_not_displayed() {
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
        // The validator never forced the opaque value through Display.
        assert_eq!(0, count.get());
    }

    #[test]
    fn opaque_debug_value_is_not_parsed_from_its_debug_string() {
        struct FakeFields;

        impl fmt::Debug for FakeFields {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                // A fake Debug string that looks like it contains fields.
                f.write_str("key = \"hacked\", value = 999")
            }
        }

        let value = Value::from_debug(&FakeFields);
        let source = [("dbg", value)];

        let report = Validator::new(&allow_all()).validate(&source);

        assert!(report.complete);
        // Classification uses the VisitValue API, not the Debug string.
        assert_eq!(ValueKind::Opaque, report.pairs[0].kind);
    }

    #[test]
    fn four_field_record_example() {
        use crate::kv::Value;

        let source = [
            ("request_id", Value::from("req-8a2f")),
            ("method", Value::from("GET")),
            ("status", Value::from(200u64)),
            ("latency_ms", Value::from(12u64)),
        ];

        let record = crate::Record::builder()
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
        assert_eq!(ValueKind::Str, report.pairs[0].kind);
        assert_eq!(ValueKind::U64, report.pairs[2].kind);
    }
}
