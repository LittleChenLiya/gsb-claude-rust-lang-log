//! Integration tests for the `log::kv::json` bridge.
//!
//! Run with: `cargo test --features kv_std --test kv_json`
#![cfg(all(feature = "kv", feature = "std"))]

use log::kv::{json, ToValue, Value};
use std::cell::Cell;
use std::fmt;

/// A short KV source with an integer, a string that needs escaping, a
/// borrowed short string, and an opaque value that counts `Debug` calls.
struct Fixture<'a> {
    debug_calls: &'a Cell<usize>,
}

impl<'a> fmt::Debug for Fixture<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.debug_calls.set(self.debug_calls.get() + 1);
        f.write_str("Fixture")
    }
}

#[test]
fn kv_json_bridge() {
    let debug_calls = Cell::new(0);

    // A borrowed short string that doesn't live for 'static.
    let borrowed = String::from("short");

    let ok_source = &[
        ("answer", 42i64.to_value()),
        ("msg", "he said \"hi\"\n".to_value()),
        ("name", (&borrowed).to_value()),
    ];

    // Scalars encode to a single line of valid JSON.
    let encoded = json::to_json_string(&ok_source).unwrap();
    assert_eq!(
        "{\"answer\":42,\"msg\":\"he said \\\"hi\\\"\\n\",\"name\":\"short\"}",
        encoded
    );
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(42, parsed["answer"]);
    assert_eq!("he said \"hi\"\n", parsed["msg"]);
    assert_eq!("short", parsed["name"]);

    // The same source also encodes through the writer-based API.
    let mut out = String::new();
    json::write_json(&mut out, &ok_source).unwrap();
    assert_eq!(encoded, out);

    // A source that also carries an opaque value.
    let opaque = Fixture {
        debug_calls: &debug_calls,
    };
    let with_opaque = &[
        ("answer", 42i64.to_value()),
        ("msg", "he said \"hi\"\n".to_value()),
        ("opaque", Value::from_debug(&opaque)),
    ];

    let mut out = String::new();
    let err = json::write_json(&mut out, &with_opaque).unwrap_err();

    // The key path is preserved in the error...
    assert_eq!(Some("opaque"), err.key());
    assert!(err.to_string().contains("opaque"));

    // ...the opaque value produced zero output (no half-written line)...
    assert_eq!("", out);

    // ...and `Debug` was never called.
    assert_eq!(0, debug_calls.get());

    // The error plugs into the existing `kv::Error` handling.
    let kv_err = log::kv::Error::from(err);
    assert!(kv_err.to_string().contains("opaque"));
}

#[test]
fn kv_json_bridge_with_record() {
    // The bridge works directly on the key-values of a log `Record`,
    // without touching the regular log dispatch.
    let kvs = [("answer", 42i64.to_value()), ("msg", "hello".to_value())];
    let record = log::Record::builder()
        .level(log::Level::Info)
        .key_values(&kvs)
        .build();

    let encoded = json::to_json_string(record.key_values()).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(42, parsed["answer"]);
    assert_eq!("hello", parsed["msg"]);
}
