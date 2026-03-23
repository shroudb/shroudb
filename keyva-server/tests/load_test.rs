#[allow(dead_code)]
mod common;
use common::{TestClient, TestServer};

/// Load test: sustained ISSUE + VERIFY workload.
///
/// Not run by default (takes >10s). Run with:
///   cargo test -p keyva --test load_test -- --ignored
#[tokio::test]
#[ignore] // Long-running — run manually
async fn sustained_load_1000_ops() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let start = std::time::Instant::now();
    let mut issued = Vec::new();

    // Issue 500 API keys
    for i in 0..500 {
        let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
        assert!(
            !resp.is_error(),
            "ISSUE {i} failed: {}",
            if resp.is_error() {
                resp.as_error()
            } else {
                "unexpected"
            }
        );
        issued.push(resp.get("api_key").as_str().to_string());
    }

    // Verify all 500
    for (i, key) in issued.iter().enumerate() {
        let resp = client.cmd(&["VERIFY", "test-apikeys", key]).await;
        assert!(
            !resp.is_error(),
            "VERIFY {i} failed: {}",
            if resp.is_error() {
                resp.as_error()
            } else {
                "unexpected"
            }
        );
    }

    let elapsed = start.elapsed();
    let ops = 1000;
    let ops_per_sec = ops as f64 / elapsed.as_secs_f64();

    println!(
        "Load test: {ops} ops in {:.2}s ({:.0} ops/sec)",
        elapsed.as_secs_f64(),
        ops_per_sec
    );

    // Should complete well under 10s for 1000 ops
    assert!(elapsed.as_secs() < 10, "load test too slow: {elapsed:?}");
}
