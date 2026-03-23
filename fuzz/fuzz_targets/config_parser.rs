#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Feed arbitrary bytes into the TOML parser.
    // Should never panic — only return Ok or Err.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = toml::from_str::<toml::Value>(s);
    }
});
