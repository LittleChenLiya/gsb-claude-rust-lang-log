//! A small demo of the `log::kv::json` bridge using in-memory and
//! temp-file fixtures.
//!
//! Run with: `cargo run --features kv_std --example kv_json`

#[cfg(all(feature = "kv", feature = "std"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use log::kv::{json, ToValue, Value};
    use std::cell::Cell;
    use std::fmt;

    // An opaque value that counts how often its `Debug` impl is called.
    struct Opaque<'a>(&'a Cell<usize>);

    impl fmt::Debug for Opaque<'_> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            self.0.set(self.0.get() + 1);
            f.write_str("Opaque")
        }
    }

    // A short KV source: an integer, a string that needs escaping, and a
    // borrowed short string that doesn't live for 'static.
    let borrowed = String::from("short");
    let source = &[
        ("answer", 42i64.to_value()),
        ("msg", "he said \"hi\"\n".to_value()),
        ("name", (&borrowed).to_value()),
    ];

    // In-memory fixture: encode into a String.
    let line = json::to_json_string(&source)?;
    println!("in-memory fixture: {line}");

    // Temp-file fixture: round-trip the line through the temp dir.
    let path = std::env::temp_dir().join("log-kv-json-demo.json");
    std::fs::write(&path, &line)?;
    let read_back = std::fs::read_to_string(&path)?;
    std::fs::remove_file(&path)?;
    println!("temp-file fixture: wrote {} bytes, round-trip ok", read_back.len());

    // A source with an opaque value: rejected, zero output, zero Debug calls.
    let debug_calls = Cell::new(0);
    let opaque = Opaque(&debug_calls);
    let with_opaque = &[
        ("answer", 42i64.to_value()),
        ("opaque", Value::from_debug(&opaque)),
    ];

    let mut out = String::new();
    match json::write_json(&mut out, &with_opaque) {
        Err(err) => println!(
            "opaque fixture: rejected at key `{}`; output bytes: {}; debug calls: {}",
            err.key().unwrap_or("<none>"),
            out.len(),
            debug_calls.get(),
        ),
        Ok(()) => println!("opaque fixture: unexpectedly encoded: {out}"),
    }

    Ok(())
}

#[cfg(not(all(feature = "kv", feature = "std")))]
fn main() {
    eprintln!("this example requires the `kv` and `std` features; run with `--features kv_std`");
}
