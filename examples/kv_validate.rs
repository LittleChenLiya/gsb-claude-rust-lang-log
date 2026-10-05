//! Validate a four-field log record's key-values.
//!
//! This example builds a [`log::Record`] with four structured key-values, then
//! runs the explicit [`log::kv::Validator`] over them. The validator visits
//! the source exactly once, preserves access order and duplicate keys,
//! classifies each value by kind without parsing `Debug` strings, and never
//! forces an opaque value through `Display`. The default log `Record` is not
//! modified.
//!
//! # Local cargo commands
//!
//! Run this example:
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

#[cfg(all(feature = "kv", feature = "alloc"))]
fn run() {
    use log::kv::{AllowedKeys, DuplicatePolicy, Validator, Value, ValueKind};
    use log::Record;

    // Four structured fields, in access order.
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

    println!("complete: {}", report.complete);
    for pair in &report.pairs {
        println!("  {} -> {:?}", pair.key, pair.kind);
    }
    if let Some(err) = &report.error {
        println!("error: {err}");
    }
}

fn main() {
    #[cfg(all(feature = "kv", feature = "alloc"))]
    run();

    #[cfg(not(all(feature = "kv", feature = "alloc")))]
    eprintln!(
        "this example requires the `kv_std` feature; run with:\n\
         cargo run --example kv_validate --features kv_std"
    );
}
