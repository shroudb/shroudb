#![no_main]
use libfuzzer_sys::fuzz_target;

use shroudb_store::{MetaSchema, metadata_from_json};

fuzz_target!(|data: &[u8]| {
    // Split input in half: first half is schema JSON, second half is metadata JSON
    if data.len() < 4 {
        return;
    }
    let mid = data.len() / 2;
    let schema_bytes = &data[..mid];
    let meta_bytes = &data[mid..];

    // Try to parse schema
    let Ok(schema_str) = std::str::from_utf8(schema_bytes) else {
        return;
    };
    let Ok(schema) = serde_json::from_str::<MetaSchema>(schema_str) else {
        return;
    };

    // Try to parse metadata
    let Ok(meta_str) = std::str::from_utf8(meta_bytes) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(meta_str) else {
        return;
    };
    let Ok(mut metadata) = metadata_from_json(json) else {
        return;
    };

    // Validate — must never panic
    let _ = schema.validate(&mut metadata);

    // Also test validate_update with the same metadata as both existing and patch
    let _ = schema.validate_update(&metadata, &metadata);
});
