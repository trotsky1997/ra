//! Emit the JSON Schema for `ra::config::RaConfig` to stdout.
//!
//! Run from the project root:
//!   cargo run --bin gen-schema > spec/ra-config.schema.json
//!
//! The output is a Draft 2020-12 schema derived directly from the
//! Rust types via `schemars`, so it's authoritative for whatever the
//! current Cargo build accepts.

fn main() {
    let schema = schemars::schema_for!(ra::config::RaConfig);
    let out = serde_json::to_string_pretty(&schema).expect("serialize JSON schema");
    println!("{out}");
}
