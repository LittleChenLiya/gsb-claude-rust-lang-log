//! A JSON bridge for structured key-values.
//!
//! This module encodes the key-values of a [`Source`] as a single JSON object,
//! using only the scalar conversions [`Value`] publicly supports. It never
//! falls back to a value's `fmt::Debug` implementation: opaque values that can
//! only be observed through `Debug`, `Display`, `serde` or `sval` are rejected
//! with an error that preserves the offending key.
//!
//! Encoding enumerates the source exactly once into an internal buffer. Each
//! value is checked before any of its bytes are produced, and the buffered
//! object is only written to the output once the whole enumeration succeeds,
//! so a failure never leaves half a line of JSON behind.
//!
//! Borrowed values are encoded in place; nothing here requires a `'static`
//! lifetime.
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), log::kv::json::JsonError> {
//! use log::kv::{json, ToValue};
//!
//! // A short string that only lives for this scope.
//! let borrowed = String::from("a borrowed string");
//!
//! // info!(answer = 42, msg = "he said \"hi\"", borrowed = borrowed; "...");
//! let kvs = &[
//!     ("answer", 42i64.to_value()),
//!     ("msg", "he said \"hi\"\n".to_value()),
//!     ("borrowed", (&borrowed).to_value()),
//! ];
//!
//! let encoded = json::to_json_string(&kvs)?;
//!
//! assert_eq!(
//!     "{\"answer\":42,\"msg\":\"he said \\\"hi\\\"\\n\",\"borrowed\":\"a borrowed string\"}",
//!     encoded,
//! );
//! # Ok(())
//! # }
//! ```

use crate::kv::{Error, Key, Source, Value, VisitSource, VisitValue};
use std::error;
use std::fmt::{self, Write};
use std::string::String;

/// An error encountered while encoding key-values as JSON.
///
/// When a value can't be encoded, the key it was captured under is preserved
/// and can be inspected through [`JsonError::key`].
#[derive(Debug)]
pub struct JsonError {
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    /// A value isn't one of the scalars this bridge encodes.
    UnsupportedValue { key: String, reason: &'static str },
    /// Enumerating the source itself failed.
    Source(Error),
    /// The underlying output writer failed.
    Fmt,
}

impl JsonError {
    fn unsupported_value(key: &str, reason: &'static str) -> Self {
        JsonError {
            kind: Kind::UnsupportedValue {
                key: String::from(key),
                reason,
            },
        }
    }

    /// The key of the value that couldn't be encoded as a JSON scalar.
    ///
    /// This returns `None` if the failure came from enumerating the source or
    /// from the output writer instead of from an unsupported value.
    pub fn key(&self) -> Option<&str> {
        match &self.kind {
            Kind::UnsupportedValue { key, .. } => Some(key),
            _ => None,
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.kind {
            Kind::UnsupportedValue { key, reason } => {
                write!(f, "the value at key `{key}` can't be encoded as JSON: {reason}")
            }
            Kind::Source(err) => write!(f, "failed to enumerate key-values: {err}"),
            Kind::Fmt => fmt::Error.fmt(f),
        }
    }
}

impl error::Error for JsonError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match &self.kind {
            Kind::Source(err) => Some(err),
            _ => None,
        }
    }
}

impl From<fmt::Error> for JsonError {
    fn from(_: fmt::Error) -> Self {
        JsonError { kind: Kind::Fmt }
    }
}

impl From<Error> for JsonError {
    fn from(err: Error) -> Self {
        JsonError {
            kind: Kind::Source(err),
        }
    }
}

impl From<JsonError> for Error {
    fn from(err: JsonError) -> Self {
        Error::boxed(err)
    }
}

/// Encode the key-values of `source` as a JSON object, writing it to `writer`.
///
/// The source is enumerated once into an internal buffer; `writer` only sees
/// the buffered object if every value is a supported scalar. If any value is
/// opaque then an error is returned and nothing is written to `writer`.
pub fn write_json<W: Write>(
    writer: &mut W,
    source: &(impl Source + ?Sized),
) -> Result<(), JsonError> {
    let mut buf = String::new();
    encode_object(&mut buf, source)?;
    writer.write_str(&buf).map_err(JsonError::from)
}

/// Encode the key-values of `source` as a JSON object, returning it as a `String`.
///
/// This is a convenience wrapper around [`write_json`].
pub fn to_json_string(source: &(impl Source + ?Sized)) -> Result<String, JsonError> {
    let mut buf = String::new();
    encode_object(&mut buf, source)?;
    Ok(buf)
}

/// Enumerate `source` once, encoding each pair into `buf`.
///
/// Each value is checked before its bytes reach `buf`, and `buf` is only
/// observed by callers if the whole enumeration succeeds, so a failure never
/// leaks a partially encoded object.
fn encode_object(buf: &mut String, source: &(impl Source + ?Sized)) -> Result<(), JsonError> {
    buf.push('{');

    let mut visitor = ObjectWriter {
        out: buf,
        first: true,
        err: None,
    };

    let visit_result = source.visit(&mut visitor);

    // A source may swallow visitor errors, so prefer the error the visitor
    // stashed (which preserves the key) over the one `visit` returned.
    if let Some(err) = visitor.err {
        return Err(err);
    }

    visit_result.map_err(JsonError::from)?;

    buf.push('}');

    Ok(())
}

/// A `VisitSource` that encodes each pair it sees into a JSON object.
struct ObjectWriter<'a> {
    out: &'a mut String,
    first: bool,
    err: Option<JsonError>,
}

impl<'a, 'kvs> VisitSource<'kvs> for ObjectWriter<'a> {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), Error> {
        // Once a pair has failed, refuse any further pairs so the source
        // stops feeding us values we've already given up on.
        if self.err.is_some() {
            return Err(Error::msg("a previous key-value failed to encode as JSON"));
        }

        if !self.first {
            self.out.push(',');
        }

        write_json_string(self.out, key.as_str());
        self.out.push(':');

        let mut scalar = ScalarWriter {
            out: self.out,
            unsupported: None,
        };

        value.visit(&mut scalar)?;

        if let Some(reason) = scalar.unsupported {
            self.err = Some(JsonError::unsupported_value(key.as_str(), reason));
            return Err(Error::msg("the value isn't a supported JSON scalar"));
        }

        self.first = false;

        Ok(())
    }
}

/// A `VisitValue` that encodes scalar values as JSON.
///
/// Only the scalar shapes `Value` publicly supports are handled. Anything
/// else falls through to `visit_any`, which records the value as unsupported
/// instead of formatting it; `Debug` is never called.
struct ScalarWriter<'a> {
    out: &'a mut String,
    unsupported: Option<&'static str>,
}

impl<'a, 'v> VisitValue<'v> for ScalarWriter<'a> {
    fn visit_any(&mut self, _: Value) -> Result<(), Error> {
        self.unsupported = Some(
            "the value is opaque; it can only be observed through `Debug`, `Display`, \
             `serde` or `sval`, none of which this bridge uses",
        );
        Ok(())
    }

    fn visit_null(&mut self) -> Result<(), Error> {
        self.out.push_str("null");
        Ok(())
    }

    fn visit_u64(&mut self, value: u64) -> Result<(), Error> {
        write!(self.out, "{value}")?;
        Ok(())
    }

    fn visit_i64(&mut self, value: i64) -> Result<(), Error> {
        write!(self.out, "{value}")?;
        Ok(())
    }

    fn visit_u128(&mut self, value: u128) -> Result<(), Error> {
        write!(self.out, "{value}")?;
        Ok(())
    }

    fn visit_i128(&mut self, value: i128) -> Result<(), Error> {
        write!(self.out, "{value}")?;
        Ok(())
    }

    fn visit_f64(&mut self, value: f64) -> Result<(), Error> {
        if value.is_finite() {
            write!(self.out, "{value}")?;
        } else {
            self.unsupported = Some("non-finite floats have no JSON representation");
        }
        Ok(())
    }

    fn visit_bool(&mut self, value: bool) -> Result<(), Error> {
        self.out.push_str(if value { "true" } else { "false" });
        Ok(())
    }

    fn visit_str(&mut self, value: &str) -> Result<(), Error> {
        write_json_string(self.out, value);
        Ok(())
    }

    fn visit_borrowed_str(&mut self, value: &'v str) -> Result<(), Error> {
        // The string is borrowed from the record for some lifetime 'v;
        // it's written out here without ever requiring 'static.
        write_json_string(self.out, value);
        Ok(())
    }

    fn visit_char(&mut self, value: char) -> Result<(), Error> {
        let mut buf = [0; 4];
        write_json_string(self.out, &*value.encode_utf8(&mut buf));
        Ok(())
    }
}

/// Write `s` to `out` as a quoted, escaped JSON string.
fn write_json_string(out: &mut String, s: &str) {
    out.push('"');

    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                // Writing to a `String` never fails.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }

    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::ToValue;
    use std::cell::Cell;

    #[test]
    fn encodes_scalars_as_valid_json() {
        let kvs = &[
            ("answer", 42i64.to_value()),
            ("negative", (-7i32).to_value()),
            ("big", 18_446_744_073_709_551_615u128.to_value()),
            ("ratio", 0.5f64.to_value()),
            ("flag", true.to_value()),
            ("letter", 'x'.to_value()),
            ("nothing", Value::null()),
        ];

        let json = to_json_string(&kvs).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(42, parsed["answer"]);
        assert_eq!(-7, parsed["negative"]);
        assert_eq!(serde_json::json!(u64::MAX), parsed["big"]);
        assert_eq!(0.5, parsed["ratio"]);
        assert_eq!(true, parsed["flag"]);
        assert_eq!("x", parsed["letter"]);
        assert_eq!(serde_json::Value::Null, parsed["nothing"]);
    }

    #[test]
    fn escapes_strings() {
        let kvs = &[("msg", "he said \"hi\"\n\t\\ \u{01}".to_value())];

        let json = to_json_string(&kvs).unwrap();

        assert_eq!("{\"msg\":\"he said \\\"hi\\\"\\n\\t\\\\ \\u0001\"}", json);

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!("he said \"hi\"\n\t\\ \u{01}", parsed["msg"]);
    }

    #[test]
    fn encodes_borrowed_strings_without_static() {
        // A short string that only lives for this scope; encoding it must
        // not require a 'static lifetime.
        let borrowed = String::from("borrowed");
        let kvs = &[("name", (&borrowed).to_value())];

        let json = to_json_string(&kvs).unwrap();

        assert_eq!("{\"name\":\"borrowed\"}", json);
    }

    #[test]
    fn rejects_opaque_values_with_key_and_no_output_or_debug() {
        struct Opaque<'a> {
            debug_calls: &'a Cell<usize>,
            display_calls: &'a Cell<usize>,
        }

        impl fmt::Debug for Opaque<'_> {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                self.debug_calls.set(self.debug_calls.get() + 1);
                f.write_str("Opaque")
            }
        }

        impl fmt::Display for Opaque<'_> {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                self.display_calls.set(self.display_calls.get() + 1);
                f.write_str("Opaque")
            }
        }

        let debug_calls = Cell::new(0);
        let display_calls = Cell::new(0);
        let opaque = Opaque {
            debug_calls: &debug_calls,
            display_calls: &display_calls,
        };

        let kvs = &[
            ("answer", 42i64.to_value()),
            ("opaque", Value::from_debug(&opaque)),
        ];

        let mut out = String::new();
        let err = write_json(&mut out, &kvs).unwrap_err();

        // The key path is preserved.
        assert_eq!(Some("opaque"), err.key());
        assert!(err.to_string().contains("opaque"));

        // Not a single byte of JSON was written.
        assert_eq!("", out);

        // Debug (and Display) were never called.
        assert_eq!(0, debug_calls.get());
        assert_eq!(0, display_calls.get());
    }

    #[test]
    fn rejects_display_only_values_as_opaque() {
        let kvs = &[("addr", Value::from_display(&std::net::Ipv4Addr::LOCALHOST))];

        let err = to_json_string(&kvs).unwrap_err();

        assert_eq!(Some("addr"), err.key());
    }

    #[test]
    fn rejects_non_finite_floats() {
        let kvs = &[("nan", f64::NAN.to_value())];

        let err = to_json_string(&kvs).unwrap_err();

        assert_eq!(Some("nan"), err.key());
    }

    #[test]
    fn converts_into_kv_error() {
        let kvs = &[("opaque", Value::from_debug(&1))];
        let err = to_json_string(&kvs).unwrap_err();

        // The bridge error plugs into the existing `kv::Error` handling.
        let kv_err = Error::from(err);
        assert!(kv_err.to_string().contains("opaque"));
    }

    #[test]
    fn writes_through_fmt_writer() {
        let kvs = &[("a", 1i64.to_value())];

        let mut out = String::new();
        write_json(&mut out, &kvs).unwrap();

        assert_eq!("{\"a\":1}", out);
    }
}
