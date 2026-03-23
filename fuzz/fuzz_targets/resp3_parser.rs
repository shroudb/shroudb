#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // &[u8] implements tokio::io::AsyncRead
        let mut reader = tokio::io::BufReader::new(data);
        let _ = keyva_protocol::resp3::reader::read_frame(&mut reader).await;
    });
});
