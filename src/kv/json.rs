//! A JSON bridge for key-values that never falls back to `Debug`.
//!
//! This module adapts the key-values on a [`Source`] to a JSON object
//! without ever calling `fmt::Debug`. Only the scalar types that
//! [`Value`] publicly supports through [`VisitValue`] are encoded:
//!
//! - `null`
//! - booleans
//! - integers (`u64`, `i64`, `u128`, `i128`)
//! - finite floats (`f64`)
//! - strings and characters
//!
//! Any other value is *opaque* to this bridge. That includes values
//! captured through `Debug`, `Display`, `Error`, `serde`, or `sval`.
//! Opaque values are explicitly rejected with an [`Error`] that keeps
//! the key path of the offending pair, instead of being silently
//! stringified. Non-finite floats (`NaN`, infinities) are also rejected
//! because they have no JSON representation.
//!
//! Encoding enumerates the source exactly once. Each pair is checked
//! before anything is written for it, so a failing pair produces zero
//! bytes of output. [`to_json_string`] buffers the whole document and
//! only returns it on success, so a failure never writes half a line
//! of JSON. Borrowed values are encoded in place; neither keys nor
//! values need a `'static` lifetime.
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), log::kv::json::Error> {
//! use log::kv::{json, Value};
//!
//! let source = [
//!     ("id", Value::from(42i64)),
//!     ("msg", Value::from("a \"quoted\" string")),
//! ];
//!
//! let json = json::to_json_string(&source)?;
//!
//! assert_eq!(r#"{"id":42,"msg":"a \"quoted\" string"}"#, json);
//! # Ok(())
//! # }
//! ```

use alloc::string::String;
use std::fmt::{self, Write as _};

use crate::kv::{Error as KvError, Key, Source, Value, VisitSource, VisitValue};

/// An error encountered while encoding key-values as JSON.
///
/// The error keeps the key path of the pair that failed to encode,
/// so callers can tell exactly which key-value was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    key: String,
    kind: Kind,
}

/// The reason a key-value failed to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The value isn't one of the scalar types `Value` publicly supports.
    ///
    /// This covers values captured through `Debug`, `Display`, `Error`,
    /// `serde`, or `sval`. They're rejected rather than formatted.
    Opaque,
    /// The value is a non-finite float, which has no JSON representation.
    NonFiniteFloat,
}

impl Error {
    fn new(key: &Key, kind: Kind) -> Self {
        Error {
            key: String::from(key.as_str()),
            kind,
        }
    }

    /// The key path of the key-value that failed to encode.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether the value was rejected because it's opaque to this bridge.
    ///
    /// Opaque values are ones that `Value` doesn't expose as a scalar,
    /// such as values captured through `Debug`, `Display`, `Error`,
    /// `serde`, or `sval`.
    pub fn is_opaque_value(&self) -> bool {
        matches!(self.kind, Kind::Opaque)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.kind {
            Kind::Opaque => write!(
                f,
                "the value at key `{}` is opaque: only scalar values can be encoded as JSON",
                self.key
            ),
            Kind::NonFiniteFloat => write!(
                f,
                "the value at key `{}` is not a finite float and cannot be encoded as JSON",
                self.key
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

#[cfg(feature = "std")]
impl From<Error> for KvError {
    fn from(err: Error) -> Self {
        KvError::boxed(err)
    }
}

/// Encode all key-values in `source` as a single JSON object.
///
/// The source is enumerated exactly once. Each pair is checked before
/// anything is written for it; if any pair fails to encode then the
/// whole document is discarded and an [`Error`] keeping the pair's key
/// path is returned. No partial JSON is ever produced.
///
/// This is the all-or-nothing counterpart of [`JsonObject`], which
/// plugs into the existing [`Source::visit`] interface.
pub fn to_json_string(source: &(impl Source + ?Sized)) -> Result<String, Error> {
    let mut object = JsonObject::new();

    // A failing pair records its `Error` in the object before the
    // `kv::Error` early-returns the enumeration, so it's never lost.
    let _ = source.visit(&mut object);

    object.finish()
}

/// A [`VisitSource`] that encodes key-values as a JSON object.
///
/// `JsonObject` adapts the existing [`Source::visit`] interface to JSON
/// output. Each pair is checked before it's written; a pair holding an
/// opaque value is rejected with a [`kv::Error`](../struct.Error.html)
/// that keeps the pair's key path, and contributes zero bytes to the
/// output. Pairs that were already committed remain in the buffer, so
/// callers that need all-or-nothing output should discard the object on
/// error or use [`to_json_string`] instead.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), log::kv::Error> {
/// use log::kv::{json::JsonObject, Source};
///
/// let source = [("a", 1), ("b", 2)];
///
/// let mut object = JsonObject::new();
/// source.visit(&mut object)?;
///
/// let json = object.finish().expect("all pairs are scalars");
///
/// assert_eq!(r#"{"a":1,"b":2}"#, json);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct JsonObject {
    buf: String,
    scratch: String,
    started: bool,
    err: Option<Error>,
}

impl JsonObject {
    /// Start a new, empty JSON object.
    pub fn new() -> Self {
        JsonObject::default()
    }

    /// The JSON encoded so far.
    ///
    /// This is only a complete document once [`JsonObject::finish`] has
    /// been called. It's useful for inspecting how much output a failed
    /// enumeration produced.
    pub fn buffer(&self) -> &str {
        &self.buf
    }

    /// Finish the object, returning the complete JSON document.
    ///
    /// If any pair failed to encode, the [`Error`] recorded for it is
    /// returned instead and the partial document is discarded.
    pub fn finish(mut self) -> Result<String, Error> {
        if let Some(err) = self.err {
            return Err(err);
        }

        if !self.started {
            self.buf.push('{');
        }
        self.buf.push('}');

        Ok(self.buf)
    }
}

impl<'kvs> VisitSource<'kvs> for JsonObject {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), KvError> {
        match encode_pair(&mut self.scratch, &key, &value) {
            Ok(()) => {
                if self.started {
                    self.buf.push(',');
                } else {
                    self.buf.push('{');
                    self.started = true;
                }

                self.buf.push_str(&self.scratch);

                Ok(())
            }
            Err(kind) => {
                let err = Error::new(&key, kind);

                #[cfg(feature = "std")]
                let kv_err = KvError::from(err.clone());

                #[cfg(not(feature = "std"))]
                let kv_err = KvError::msg("a key-value could not be encoded as JSON");

                // Keep the full error (with the key path) for `finish`,
                // even where a `kv::Error` can't carry it.
                self.err = Some(err);

                Err(kv_err)
            }
        }
    }
}

/// Check a pair and, if it's encodable, write it as `"key":value`.
///
/// The check runs before any output, so a rejected pair leaves
/// `scratch` untouched by its value.
fn encode_pair(scratch: &mut String, key: &Key, value: &Value) -> Result<(), Kind> {
    check_value(value)?;

    scratch.clear();
    escape_str(scratch, key.as_str());
    scratch.push(':');
    encode_value(scratch, value)
}

/// Check that a value is a scalar this bridge can encode, without
/// producing any output. Never formats the value.
fn check_value(value: &Value) -> Result<(), Kind> {
    struct Check(Option<Kind>);

    impl<'v> VisitValue<'v> for Check {
        fn visit_any(&mut self, _: Value) -> Result<(), KvError> {
            self.0 = Some(Kind::Opaque);
            Err(KvError::msg("opaque values cannot be encoded as JSON"))
        }

        fn visit_null(&mut self) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_u64(&mut self, _: u64) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_i64(&mut self, _: i64) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_u128(&mut self, _: u128) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_i128(&mut self, _: i128) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_f64(&mut self, value: f64) -> Result<(), KvError> {
            if value.is_finite() {
                Ok(())
            } else {
                self.0 = Some(Kind::NonFiniteFloat);
                Err(KvError::msg("non-finite floats cannot be encoded as JSON"))
            }
        }

        fn visit_bool(&mut self, _: bool) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_str(&mut self, _: &str) -> Result<(), KvError> {
            Ok(())
        }

        fn visit_char(&mut self, _: char) -> Result<(), KvError> {
            Ok(())
        }
    }

    let mut check = Check(None);
    let _ = value.visit(&mut check);

    match check.0 {
        Some(kind) => Err(kind),
        None => Ok(()),
    }
}

/// Encode a checked value as a JSON scalar. Never formats the value
/// through `Debug` or `Display`.
fn encode_value(buf: &mut String, value: &Value) -> Result<(), Kind> {
    struct Encode<'a> {
        buf: &'a mut String,
        err: Option<Kind>,
    }

    impl VisitValue<'_> for Encode<'_> {
        fn visit_any(&mut self, _: Value) -> Result<(), KvError> {
            self.err = Some(Kind::Opaque);
            Err(KvError::msg("opaque values cannot be encoded as JSON"))
        }

        fn visit_null(&mut self) -> Result<(), KvError> {
            self.buf.push_str("null");
            Ok(())
        }

        fn visit_u64(&mut self, value: u64) -> Result<(), KvError> {
            let _ = write!(self.buf, "{value}");
            Ok(())
        }

        fn visit_i64(&mut self, value: i64) -> Result<(), KvError> {
            let _ = write!(self.buf, "{value}");
            Ok(())
        }

        fn visit_u128(&mut self, value: u128) -> Result<(), KvError> {
            let _ = write!(self.buf, "{value}");
            Ok(())
        }

        fn visit_i128(&mut self, value: i128) -> Result<(), KvError> {
            let _ = write!(self.buf, "{value}");
            Ok(())
        }

        fn visit_f64(&mut self, value: f64) -> Result<(), KvError> {
            if value.is_finite() {
                let _ = write!(self.buf, "{value}");
                Ok(())
            } else {
                self.err = Some(Kind::NonFiniteFloat);
                Err(KvError::msg("non-finite floats cannot be encoded as JSON"))
            }
        }

        fn visit_bool(&mut self, value: bool) -> Result<(), KvError> {
            self.buf.push_str(if value { "true" } else { "false" });
            Ok(())
        }

        fn visit_str(&mut self, value: &str) -> Result<(), KvError> {
            escape_str(self.buf, value);
            Ok(())
        }

        fn visit_char(&mut self, value: char) -> Result<(), KvError> {
            let mut b = [0; 4];
            escape_str(self.buf, &*value.encode_utf8(&mut b));
            Ok(())
        }
    }

    let mut encode = Encode { buf, err: None };
    let _ = value.visit(&mut encode);

    match encode.err {
        Some(kind) => Err(kind),
        None => Ok(()),
    }
}

/// Write `s` as a JSON string literal, escaping as needed.
fn escape_str(buf: &mut String, s: &str) {
    buf.push('"');

    for c in s.chars() {
        match c {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            '\u{08}' => buf.push_str("\\b"),
            '\u{0c}' => buf.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => buf.push(c),
        }
    }

    buf.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::{ToValue, Value};

    use std::fmt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A value whose `Debug` implementation counts its calls, so tests
    /// can prove the JSON bridge never invokes it.
    struct CountedDebug;

    static DEBUG_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl fmt::Debug for CountedDebug {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            DEBUG_CALLS.fetch_add(1, Ordering::SeqCst);
            f.write_str("CountedDebug")
        }
    }

    fn debug_calls() -> usize {
        DEBUG_CALLS.load(Ordering::SeqCst)
    }

    #[test]
    fn scalars_encode_as_valid_json() {
        let source = [
            ("int", 42i64.to_value()),
            ("neg", (-7i32).to_value()),
            ("big", u128::MAX.to_value()),
            ("float", 1.5f64.to_value()),
            ("bool", true.to_value()),
            ("char", '"'.to_value()),
            (
                "escaped",
                "quote\" backslash\\ newline\n tab\t ctrl\u{1f}".to_value(),
            ),
            ("null", Value::null()),
        ];

        let json = to_json_string(&source).unwrap();

        assert_eq!(
            concat!(
                "{\"int\":42,\"neg\":-7,",
                "\"big\":340282366920938463463374607431768211455,",
                "\"float\":1.5,\"bool\":true,\"char\":\"\\\"\",",
                "\"escaped\":\"quote\\\" backslash\\\\ newline\\n tab\\t ctrl\\u001f\",",
                "\"null\":null}"
            ),
            json
        );

        // The output is valid JSON that a real parser accepts.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(42, parsed["int"]);
        assert_eq!(-7, parsed["neg"]);
        assert!(parsed["big"].is_number());
        assert_eq!(1.5, parsed["float"]);
        assert_eq!(true, parsed["bool"]);
        assert_eq!("\"", parsed["char"]);
        assert_eq!(
            "quote\" backslash\\ newline\n tab\t ctrl\u{1f}",
            parsed["escaped"]
        );
        assert!(parsed["null"].is_null());
    }

    #[test]
    fn borrowed_str_does_not_require_static() {
        // A short string borrowed from a local with a non-'static lifetime.
        let text = String::from("borrowed text");
        let source = [("k", Value::from(text.as_str()))];

        let json = to_json_string(&source).unwrap();

        assert_eq!(r#"{"k":"borrowed text"}"#, json);
    }

    #[test]
    fn opaque_value_is_rejected_with_key_path_and_zero_output() {
        let calls_before = debug_calls();

        let source = [
            ("a", 1i32.to_value()),
            ("secret", Value::from_debug(&CountedDebug)),
            ("b", 2i32.to_value()),
        ];

        // The whole document is discarded: no half-written line of JSON.
        let err = to_json_string(&source).unwrap_err();

        // The key path of the opaque value is kept.
        assert_eq!("secret", err.key());
        assert!(err.is_opaque_value());
        assert!(err.to_string().contains("secret"));

        // `Debug` was never called, even though the value was rejected.
        assert_eq!(calls_before, debug_calls());

        // Streaming through the existing `VisitSource` interface: the
        // opaque pair contributes zero bytes and the error keeps its key.
        let mut object = JsonObject::new();
        object.visit_pair(Key::from("a"), 1i32.to_value()).unwrap();

        let before = String::from(object.buffer());

        let kv_err = object
            .visit_pair(Key::from("secret"), Value::from_debug(&CountedDebug))
            .unwrap_err();

        assert_eq!(before, object.buffer());
        assert_eq!("{\"a\":1", object.buffer());

        #[cfg(feature = "std")]
        assert!(kv_err.to_string().contains("secret"));

        let err = object.finish().unwrap_err();
        assert_eq!("secret", err.key());

        assert_eq!(calls_before, debug_calls());
    }

    #[test]
    fn display_and_error_captured_values_are_opaque() {
        let source = [("displayed", Value::from_display(&1i32))];

        let err = to_json_string(&source).unwrap_err();

        assert_eq!("displayed", err.key());
        assert!(err.is_opaque_value());
    }

    #[test]
    fn non_finite_float_is_rejected_with_key_path() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let source = [("ratio", value.to_value())];

            let err = to_json_string(&source).unwrap_err();

            assert_eq!("ratio", err.key());
            assert!(!err.is_opaque_value());
        }
    }

    #[test]
    fn empty_source_encodes_as_empty_object() {
        let source: [(&str, Value); 0] = [];

        assert_eq!("{}", to_json_string(&source).unwrap());
        assert_eq!("{}", JsonObject::new().finish().unwrap());
    }

    #[test]
    fn log_record_key_values_encode_through_existing_interface() {
        // A small in-memory fixture standing in for a logger's output:
        // records flow through the existing, unchanged `Record`/`Source`
        // dispatch and their key-values are collected as JSON lines.
        let mut lines: Vec<String> = Vec::new();

        let name = String::from("jane");
        let kvs = [
            ("uid", 7i64.to_value()),
            ("name", Value::from(name.as_str())),
        ];
        let record = crate::Record::builder()
            .args(format_args!("user logged in"))
            .key_values(&kvs)
            .build();

        lines.push(to_json_string(record.key_values()).unwrap());

        let opaque_kvs = [("payload", Value::from_debug(&CountedDebug))];
        let opaque = crate::Record::builder()
            .args(format_args!("opaque payload"))
            .key_values(&opaque_kvs)
            .build();

        match to_json_string(opaque.key_values()) {
            Ok(line) => lines.push(line),
            Err(err) => lines.push(format!("dropped: {err}")),
        }

        assert_eq!(r#"{"uid":7,"name":"jane"}"#, lines[0]);
        assert_eq!(
            "dropped: the value at key `payload` is opaque: \
             only scalar values can be encoded as JSON",
            lines[1]
        );

        // The actual results, visible with `cargo test -- --nocapture`.
        for line in &lines {
            println!("{line}");
        }
    }
}
