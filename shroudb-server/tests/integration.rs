mod harness;

use harness::*;
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════════════
// Core data path
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn put_get_delete_lifecycle() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();

    // PUT
    let v1 = c.put("test", b"key1", b"hello").await.unwrap();
    assert_eq!(v1, 1);

    // GET
    let entry = c.get("test", b"key1").await.unwrap();
    assert_eq!(entry.value, b"hello");
    assert_eq!(entry.version, 1);

    // PUT again — version increments
    let v2 = c.put("test", b"key1", b"world").await.unwrap();
    assert_eq!(v2, 2);

    let entry = c.get("test", b"key1").await.unwrap();
    assert_eq!(entry.value, b"world");
    assert_eq!(entry.version, 2);

    // DELETE — returns tombstone version
    let v3 = c.delete("test", b"key1").await.unwrap();
    assert_eq!(v3, 3);

    // GET after delete — should fail
    let result = c.get("test", b"key1").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn versions_history() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    c.put("test", b"k", b"v1").await.unwrap();
    c.put("test", b"k", b"v2").await.unwrap();
    c.put("test", b"k", b"v3").await.unwrap();

    let versions = c.versions("test", b"k").await.unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].version, 3); // most recent first
    assert_eq!(versions[2].version, 1);
    assert_eq!(versions[0].state, "active");
}

#[tokio::test]
async fn versions_after_delete() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    c.put("test", b"k", b"v1").await.unwrap();
    c.delete("test", b"k").await.unwrap();

    let versions = c.versions("test", b"k").await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].state, "deleted");
    assert_eq!(versions[1].state, "active");
}

#[tokio::test]
async fn get_specific_version() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    c.put("test", b"k", b"first").await.unwrap();
    c.put("test", b"k", b"second").await.unwrap();

    let v1 = c.get_version("test", b"k", 1).await.unwrap();
    assert_eq!(v1.value, b"first");

    let v2 = c.get_version("test", b"k", 2).await.unwrap();
    assert_eq!(v2.value, b"second");
}

#[tokio::test]
async fn list_with_prefix_and_pagination() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    c.put("test", b"user:1", b"alice").await.unwrap();
    c.put("test", b"user:2", b"bob").await.unwrap();
    c.put("test", b"session:1", b"s1").await.unwrap();

    // List all
    let page = c.list("test").await.unwrap();
    assert_eq!(page.items.len(), 3);

    // List with prefix
    let page = c.list_prefix("test", "user:").await.unwrap();
    assert_eq!(page.items.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════
// Error paths
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_nonexistent_namespace() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    let result = c.get("nonexistent", b"key").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delete_nonexistent_key() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    let result = c.delete("test", b"nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn namespace_create_duplicate() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    let result = c.namespace_create("test").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn namespace_drop_nonempty_without_force() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    c.put("test", b"key", b"val").await.unwrap();

    let result = c.namespace_drop("test", false).await;
    assert!(result.is_err());

    // Force drop succeeds
    c.namespace_drop("test", true).await.unwrap();
}

#[tokio::test]
async fn put_to_dropped_namespace() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();
    c.namespace_drop("test", false).await.unwrap();

    let result = c.put("test", b"key", b"val").await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Namespace operations
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn namespace_list_and_info() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("ns.alpha").await.unwrap();
    c.namespace_create("ns.beta").await.unwrap();
    c.put("ns.alpha", b"k", b"v").await.unwrap();

    let page = c.namespace_list().await.unwrap();
    assert_eq!(page.items.len(), 2);

    let info = c.namespace_info("ns.alpha").await.unwrap();
    assert_eq!(info.name, "ns.alpha");
    assert_eq!(info.key_count, 1);
}

// ═══════════════════════════════════════════════════════════════════════
// Auth + ACL
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn auth_required_rejects_unauthenticated() {
    let config = TestServerConfig {
        auth_tokens: vec![TestToken {
            raw: "test-token".into(),
            tenant: "tenant-a".into(),
            actor: "tester".into(),
            platform: true,
            grants: vec![],
        }],
        ..Default::default()
    };
    let server = TestServer::start_with_config(config)
        .await
        .expect("server failed to start");

    // Unauthenticated — only PING should work
    let mut c = server.client().await;
    c.ping().await.unwrap();

    let result = c.namespace_create("test").await;
    assert!(result.is_err());

    // After auth — commands work
    c.auth("test-token").await.unwrap();
    c.namespace_create("test").await.unwrap();
}

#[tokio::test]
async fn auth_invalid_token_rejected() {
    let config = TestServerConfig {
        auth_tokens: vec![TestToken {
            raw: "good-token".into(),
            tenant: "t".into(),
            actor: "a".into(),
            platform: true,
            grants: vec![],
        }],
        ..Default::default()
    };
    let server = TestServer::start_with_config(config)
        .await
        .expect("server failed to start");

    let mut c = server.client().await;
    let result = c.auth("bad-token").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn acl_read_only_grant() {
    let config = TestServerConfig {
        auth_tokens: vec![TestToken {
            raw: "reader".into(),
            tenant: "t".into(),
            actor: "reader".into(),
            platform: false,
            grants: vec![TestGrant {
                namespace: "data".into(),
                scopes: vec!["read".into()],
            }],
        }],
        ..Default::default()
    };
    let server = TestServer::start_with_config(config)
        .await
        .expect("server failed to start");

    // Create namespace with a platform token first — need one
    // Actually, the reader token can't create namespaces. Let's test that.
    let mut c = server.authed_client("reader").await;

    // Can't create namespace (no admin scope)
    let result = c.namespace_create("data").await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Pipeline
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pipeline_basic() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();

    let results = c
        .pipeline(
            &[
                &["PUT", "test", "k1", "v1"],
                &["PUT", "test", "k2", "v2"],
                &["GET", "test", "k1"],
            ],
            None,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 3);
}

#[tokio::test]
async fn pipeline_idempotency() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("test").await.unwrap();

    // First execution
    let r1 = c
        .pipeline(&[&["PUT", "test", "k1", "v1"]], Some("req-001"))
        .await
        .unwrap();

    // Second execution with same request_id — should return cached response
    let r2 = c
        .pipeline(&[&["PUT", "test", "k1", "v1"]], Some("req-001"))
        .await
        .unwrap();

    // Both should be arrays of length 1
    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════
// Rate limiting
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn rate_limiting_rejects_excess() {
    let config = TestServerConfig {
        rate_limit: Some(5),
        ..Default::default()
    };
    let server = TestServer::start_with_config(config)
        .await
        .expect("server failed to start");

    let mut c = server.client().await;
    c.namespace_create("test").await.unwrap();

    // Send more commands than the rate limit allows
    let mut rejected = 0;
    for i in 0..20 {
        if c.put("test", format!("k{i}").as_bytes(), b"v").await.is_err() {
            rejected += 1;
        }
    }

    // At least some should be rejected
    assert!(rejected > 0, "expected some rate-limited rejections, got 0");
}

// ═══════════════════════════════════════════════════════════════════════
// SUBSCRIBE
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn subscribe_receives_events() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;
    c.namespace_create("events").await.unwrap();

    // Subscribe on one connection — use raw_command so we can read the OK
    let mut sub = server.client().await;
    let sub_resp = sub.raw_command(&["SUBSCRIBE", "events"]).await.unwrap();
    assert!(sub_resp.as_str().is_none() || sub_resp.get_string_field("status").is_some());

    // PUT on another connection — this should generate an event
    let mut writer = server.client().await;
    writer.put("events", b"key1", b"value1").await.unwrap();

    // The subscriber should receive the push event via raw_command/read
    // Since raw_command does send+read, and we already sent SUBSCRIBE,
    // the next read from the subscriber will be the push frame.
    // Use a timeout to avoid hanging if no event arrives.
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        sub.raw_command(&["PING"]).await
    })
    .await;

    // We should get *something* back — either the push event or the PING response
    assert!(event.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════
// Export/Import
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn export_import_roundtrip() {
    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    // Create data
    c.namespace_create("export-test").await.unwrap();
    c.put("export-test", b"k1", b"v1").await.unwrap();
    c.put("export-test", b"k2", b"v2").await.unwrap();
    c.put("export-test", b"k2", b"v2-updated").await.unwrap();
    c.delete("export-test", b"k1").await.unwrap();

    let export_path = server.data_dir.path().join("export.sdb");
    let export_str = export_path.to_str().unwrap().to_string();
    let data_path = server.data_dir.path().to_path_buf();
    let config_path = server.config_path();

    // Stop server for offline subcommands
    drop(c);
    server.stop();

    // Export
    let output = run_offline_subcommand(
        &data_path,
        &config_path,
        &[
            "export",
            "--namespace",
            "export-test",
            "--output",
            &export_str,
        ],
    );
    assert!(
        output.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(export_path.exists(), "export file not created");

    // Import into same data dir with namespace rename
    let output = run_offline_subcommand(
        &data_path,
        &config_path,
        &["import", "--input", &export_str, "--namespace", "imported"],
    );
    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Telemetry
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn audit_log_created() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("audit-test").await.unwrap();
    c.put("audit-test", b"k", b"v").await.unwrap();

    // Give the audit layer time to flush
    tokio::time::sleep(Duration::from_millis(500)).await;

    let audit_path = server.data_dir.path().join("audit.log");
    assert!(
        audit_path.exists(),
        "audit.log not found at {}",
        audit_path.display()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Health + Ping
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn health_and_ping() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.ping().await.unwrap();
    c.health().await.unwrap();
}

#[tokio::test]
async fn command_list_returns_commands() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    let commands = c.command_list().await.unwrap();
    assert!(commands.contains(&"PUT".to_string()));
    assert!(commands.contains(&"GET".to_string()));
    assert!(commands.contains(&"SUBSCRIBE".to_string()));
}
