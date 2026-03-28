#![no_main]
use libfuzzer_sys::fuzz_target;

use shroudb_protocol::resp3::Resp3Frame;

fuzz_target!(|data: &[u8]| {
    // Strategy: interpret fuzz input as a sequence of strings, build a RESP3
    // array frame, and feed it to the command parser. This tests the full
    // parse_command path with arbitrary command names and argument combinations.

    // Split on null bytes to get "arguments"
    let args: Vec<Vec<u8>> = data
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_vec())
        .collect();

    if args.is_empty() {
        return;
    }

    let frame = Resp3Frame::Array(
        args.into_iter()
            .map(Resp3Frame::BulkString)
            .collect(),
    );

    // Must never panic — only Ok or Err
    let _ = shroudb_protocol::resp3::parse_command::parse_command(frame);
});
