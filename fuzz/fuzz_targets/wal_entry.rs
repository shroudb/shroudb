#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Feed arbitrary bytes into the WAL entry decoder.
    // Should never panic — only return Ok or Err.
    let _ = keyva_storage::wal::WalEntry::decode(data);
});
