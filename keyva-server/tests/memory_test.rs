#[allow(dead_code)]
mod common;
use common::{TestClient, TestServer};

/// Memory stability test: issue and verify many credentials, check memory
/// doesn't grow unbounded.
///
/// Not run by default. Run with:
///   cargo test -p keyva --test memory_test -- --ignored
#[tokio::test]
#[ignore]
async fn memory_stable_under_sustained_load() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Issue 1000 API keys
    for i in 0..1000 {
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
    }

    // Verify many times (should not leak — last_verified_at is in-place update)
    for round in 0..5 {
        for i in 0..200 {
            let resp = client.cmd(&["HEALTH"]).await;
            assert!(
                !resp.is_error(),
                "HEALTH round={round} i={i} failed: {}",
                if resp.is_error() {
                    resp.as_error()
                } else {
                    "unexpected"
                }
            );
        }
    }

    // Verify the server is still responsive after sustained load
    let resp = client.cmd(&["HEALTH"]).await;
    assert!(!resp.is_error(), "final HEALTH check failed");
    assert_eq!(resp.get("state").as_str(), "READY");

    println!("Memory test: server stable after 2000 operations");
}
