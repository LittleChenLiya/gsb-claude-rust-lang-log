//! End-to-end fixture for the `kv::json` bridge.
//!
//! A small in-memory logger collects JSON lines produced from the
//! key-values of real log records. The log facade's dispatch is used
//! unchanged; only the formatting stage is new.
#![cfg(all(feature = "kv", feature = "std"))]

use log::{info, kv::json, Level, Log, Metadata, Record};
use std::sync::{Mutex, Once};

static LOGGER: JsonLogger = JsonLogger {
    lines: Mutex::new(Vec::new()),
};

struct JsonLogger {
    lines: Mutex<Vec<String>>,
}

impl Log for JsonLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let line = match json::to_json_string(record.key_values()) {
            Ok(json) => json,
            // Opaque pairs drop the record's key-values, keeping the key path.
            Err(err) => format!("dropped: {err}"),
        };

        self.lines.lock().unwrap().push(line);
    }

    fn flush(&self) {}
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    });
}

#[test]
fn kv_json_bridge_through_log_dispatch() {
    init();

    // A short KV source: an integer, a string that needs escaping, and
    // a short borrowed string. None of them require `'static`.
    let name = String::from("ja\"ne");
    info!(uid = 7, name = name.as_str(); "user logged in");

    // An opaque value captured through `Debug`: rejected, with zero
    // output for it and without ever calling `Debug`.
    struct Opaque;
    impl std::fmt::Debug for Opaque {
        fn fmt(&self, _: &mut std::fmt::Formatter) -> std::fmt::Result {
            panic!("the JSON bridge must never call Debug")
        }
    }
    info!(payload:? = Opaque; "opaque payload");

    let lines = LOGGER.lines.lock().unwrap();

    assert_eq!(r#"{"uid":7,"name":"ja\"ne"}"#, lines[0]);
    assert_eq!(
        "dropped: the value at key `payload` is opaque: \
         only scalar values can be encoded as JSON",
        lines[1]
    );

    // Both lines that are JSON are valid JSON.
    serde_json::from_str::<serde_json::Value>(&lines[0]).unwrap();

    // The actual results, visible with `cargo test -- --nocapture`.
    for line in lines.iter() {
        println!("{line}");
    }
}

#[test]
fn kv_json_bridge_keeps_level_and_message_dispatch() {
    init();

    // The existing dispatch is untouched: levels and messages still
    // flow through `Log::log` exactly as before.
    #[derive(Default)]
    struct Probe;
    impl Log for Probe {
        fn enabled(&self, _: &Metadata) -> bool {
            true
        }
        fn log(&self, record: &Record) {
            assert_eq!(Level::Info, record.level());
            assert_eq!("plain message", record.args().to_string());
        }
        fn flush(&self) {}
    }

    let probe = Probe;
    probe.log(
        &Record::builder()
            .level(Level::Info)
            .args(format_args!("plain message"))
            .build(),
    );
}
