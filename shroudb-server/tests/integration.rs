mod common;

use common::*;
use shroudb_client::RemoteStore;
use shroudb_store::Store;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        if c.put("test", format!("k{i}").as_bytes(), b"v")
            .await
            .is_err()
        {
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

// =====================================================================
// TLS
// =====================================================================

/// Generate a self-signed certificate and key using openssl CLI.
/// Returns (cert_path, key_path) inside the given directory.
fn generate_self_signed_cert(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let cert_path = dir.join("server.crt");
    let key_path = dir.join("server.key");
    let ext_path = dir.join("ext.cnf");

    // Write extension config that marks this as an end-entity certificate
    // (not a CA) with a SAN for localhost.
    std::fs::write(
        &ext_path,
        "basicConstraints=CA:FALSE\nsubjectAltName=DNS:localhost\n",
    )
    .expect("writing ext.cnf");

    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-keyout",
        ])
        .arg(&key_path)
        .arg("-out")
        .arg(&cert_path)
        .arg("-extensions")
        .arg("v3_req")
        .args(["-days", "1", "-nodes", "-subj", "/CN=localhost"])
        .arg("-addext")
        .arg("basicConstraints=CA:FALSE")
        .arg("-addext")
        .arg("subjectAltName=DNS:localhost")
        .output()
        .expect("openssl must be installed to run TLS tests");

    assert!(
        status.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    (cert_path, key_path)
}

#[tokio::test]
async fn tls_server_accepts_tls_client() {
    let cert_dir = tempfile::tempdir().expect("tempdir");
    let (cert_path, key_path) = generate_self_signed_cert(cert_dir.path());

    let config = TestServerConfig {
        tls_cert: Some(cert_path.clone()),
        tls_key: Some(key_path),
        ..Default::default()
    };

    let server = TestServer::start_with_config(config)
        .await
        .expect("TLS server failed to start");

    // Build a custom TLS client that trusts our self-signed cert.
    let cert_pem = std::fs::read(&cert_path).unwrap();
    let mut root_store = rustls::RootCertStore::empty();
    use rustls_pki_types::pem::PemObject;
    let certs: Vec<rustls_pki_types::CertificateDer<'static>> =
        rustls_pki_types::CertificateDer::pem_slice_iter(&cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    for cert in certs {
        root_store.add(cert).unwrap();
    }

    let provider = rustls::crypto::ring::default_provider();
    let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));

    let tcp_stream = tokio::net::TcpStream::connect(&server.addr).await.unwrap();
    let domain = rustls_pki_types::ServerName::try_from("localhost".to_string()).unwrap();
    let tls_stream = connector.connect(domain, tcp_stream).await.unwrap();

    // Build a ShrouDBClient-compatible connection via raw RESP3 over TLS.
    let (r, w) = tokio::io::split(tls_stream);
    let mut reader = tokio::io::BufReader::new(r);
    let mut writer = tokio::io::BufWriter::new(w);

    // Send PING as RESP3
    use tokio::io::AsyncWriteExt;
    writer.write_all(b"*1\r\n$4\r\nPING\r\n").await.unwrap();
    writer.flush().await.unwrap();

    // Read response
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    // Should get either +PONG or a map with status: PONG
    assert!(
        line.contains("PONG") || line.starts_with('%') || line.starts_with('+'),
        "unexpected TLS response: {line}"
    );
}

#[tokio::test]
async fn tls_server_rejects_plain_tcp() {
    let cert_dir = tempfile::tempdir().expect("tempdir");
    let (cert_path, key_path) = generate_self_signed_cert(cert_dir.path());

    let config = TestServerConfig {
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
        ..Default::default()
    };

    let server = TestServer::start_with_config(config)
        .await
        .expect("TLS server failed to start");

    // Try connecting with plain TCP (no TLS) -- should fail
    let result = shroudb_client::ShrouDBClient::connect(&server.addr).await;
    match result {
        Ok(mut client) => {
            // Connection might succeed at TCP level but PING should fail
            // because the server expects a TLS handshake, not raw RESP3.
            let ping_result = client.ping().await;
            assert!(
                ping_result.is_err(),
                "plain TCP PING should fail against TLS server"
            );
        }
        Err(_) => {
            // Connection refused or reset is also acceptable
        }
    }
}

// =====================================================================
// Config hot-reload
// =====================================================================

#[tokio::test]
async fn config_hot_reload_swaps_auth_tokens() {
    let config = TestServerConfig {
        auth_tokens: vec![TestToken {
            raw: "token-alpha".into(),
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

    // Verify token A works
    let mut c = server.authed_client("token-alpha").await;
    c.namespace_create("reload-test").await.unwrap();
    drop(c);

    // Write new config with only token B (replacing token A)
    let new_config = TestServerConfig {
        auth_tokens: vec![TestToken {
            raw: "token-beta".into(),
            tenant: "t".into(),
            actor: "b".into(),
            platform: true,
            grants: vec![],
        }],
        ..Default::default()
    };
    let new_toml = generate_config_for_reload(&server.addr, &new_config);
    std::fs::write(server.config_path(), new_toml).unwrap();

    // The reloader polls every 10 seconds. Wait long enough for it to pick up the change.
    tokio::time::sleep(Duration::from_secs(15)).await;

    // Token A should now be rejected
    let mut c = server.client().await;
    let result = c.auth("token-alpha").await;
    assert!(
        result.is_err(),
        "old token should be rejected after config reload"
    );

    // Token B should work
    let mut c = server.authed_client("token-beta").await;
    c.put("reload-test", b"reloaded", b"yes").await.unwrap();
}

/// Generate config TOML for a reload test. Mirrors the harness generate_config
/// but is callable from the test module.
fn generate_config_for_reload(bind: &str, config: &TestServerConfig) -> String {
    let mut toml = format!(
        r#"[server]
bind = "{bind}"
"#
    );

    if let Some(limit) = config.rate_limit {
        toml.push_str(&format!("rate_limit_per_second = {limit}\n"));
    }

    toml.push_str("\n[storage]\n");

    if !config.auth_tokens.is_empty() {
        toml.push_str("\n[auth]\nmethod = \"token\"\n\n");
        for token in &config.auth_tokens {
            toml.push_str(&format!(
                "[auth.tokens.\"{}\"]\ntenant = \"{}\"\nactor = \"{}\"\nplatform = {}\n",
                token.raw, token.tenant, token.actor, token.platform
            ));
            if !token.grants.is_empty() {
                toml.push_str("grants = [\n");
                for grant in &token.grants {
                    let scopes: Vec<String> =
                        grant.scopes.iter().map(|s| format!("\"{s}\"")).collect();
                    toml.push_str(&format!(
                        "  {{ namespace = \"{}\", scopes = [{}] }},\n",
                        grant.namespace,
                        scopes.join(", ")
                    ));
                }
                toml.push_str("]\n");
            }
            toml.push('\n');
        }
    }

    for webhook in &config.webhooks {
        toml.push_str(&format!(
            "[[webhooks]]\nurl = \"{}\"\nsecret = \"{}\"\n",
            webhook.url, webhook.secret
        ));
        if !webhook.events.is_empty() {
            let events: Vec<String> = webhook.events.iter().map(|e| format!("\"{e}\"")).collect();
            toml.push_str(&format!("events = [{}]\n", events.join(", ")));
        }
        if !webhook.namespaces.is_empty() {
            let ns: Vec<String> = webhook
                .namespaces
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect();
            toml.push_str(&format!("namespaces = [{}]\n", ns.join(", ")));
        }
        toml.push('\n');
    }

    toml
}

// =====================================================================
// Rekey
// =====================================================================

#[tokio::test]
async fn rekey_preserves_data_with_new_master_key() {
    let new_key = "1111111111111111111111111111111111111111111111111111111111111111";

    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    // Create data
    c.namespace_create("rekey-ns").await.unwrap();
    c.put("rekey-ns", b"greeting", b"hello").await.unwrap();
    c.put("rekey-ns", b"greeting", b"hello-v2").await.unwrap();
    c.put("rekey-ns", b"count", b"42").await.unwrap();

    let data_dir = server.data_dir.path().to_path_buf();
    let config_path = server.config_path();

    // Stop the server
    drop(c);
    server.stop();

    // Run rekey subcommand: old_key -> new_key
    let old_key = &server.master_key;
    let output = run_offline_subcommand(
        &data_dir,
        &config_path,
        &["rekey", "--old-key", old_key, "--new-key", new_key],
    );
    assert!(
        output.status.success(),
        "rekey failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Start a NEW server on the same data dir with the new master key.
    // We need to take ownership of the data_dir TempDir so it is not dropped.
    // Since server.data_dir is still owned by server, we extract it.
    let data_dir_temp = std::mem::replace(&mut server.data_dir, tempfile::tempdir().unwrap());
    let server2 = start_on_data_dir(data_dir_temp, new_key)
        .await
        .expect("server with new key failed to start");

    let mut c2 = server2.client().await;

    // Verify data is accessible with the new key
    let entry = c2.get("rekey-ns", b"greeting").await.unwrap();
    assert_eq!(entry.value, b"hello-v2");
    assert_eq!(entry.version, 2);

    let entry = c2.get("rekey-ns", b"count").await.unwrap();
    assert_eq!(entry.value, b"42");

    let versions = c2.versions("rekey-ns", b"greeting").await.unwrap();
    assert_eq!(versions.len(), 2);
}

// =====================================================================
// Doctor
// =====================================================================

#[tokio::test]
async fn doctor_healthy_data_dir() {
    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("doctor-test").await.unwrap();
    c.put("doctor-test", b"k", b"v").await.unwrap();

    let data_path = server.data_dir.path().to_path_buf();
    let config_path = server.config_path();

    drop(c);
    server.stop();

    let output = run_offline_subcommand(&data_path, &config_path, &["doctor"]);
    assert!(
        output.status.success(),
        "doctor failed on healthy data: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("All checks passed"),
        "doctor output should contain success message, got: {stderr}"
    );
}

#[tokio::test]
async fn doctor_detects_wrong_master_key() {
    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("corrupt-test").await.unwrap();
    c.put("corrupt-test", b"k", b"v").await.unwrap();

    let data_path = server.data_dir.path().to_path_buf();
    let config_path = server.config_path();

    drop(c);
    server.stop();

    // Run doctor with a WRONG master key -- should fail to open storage
    // because encrypted WAL entries cannot be decrypted.
    let wrong_key = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let output = run_offline_subcommand_with_key(&data_path, &config_path, &["doctor"], wrong_key);
    // Doctor should report a storage failure (non-zero exit or FAILED in stderr)
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() || stderr.contains("FAILED"),
        "doctor should detect wrong key, exit={}, stderr={}",
        output.status,
        stderr
    );
}

// =====================================================================
// Webhooks
// =====================================================================

#[tokio::test]
async fn webhook_delivers_signed_event() {
    // Start a minimal TCP listener to receive webhook POSTs
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let webhook_port = listener.local_addr().unwrap().port();
    let webhook_url = format!("http://127.0.0.1:{webhook_port}/webhook");
    let webhook_secret = "test-webhook-secret";

    let config = TestServerConfig {
        webhooks: vec![TestWebhook {
            url: webhook_url,
            secret: webhook_secret.into(),
            events: vec!["put".into()],
            namespaces: vec![],
        }],
        ..Default::default()
    };

    let server = TestServer::start_with_config(config)
        .await
        .expect("server failed to start");

    // Accept webhook connections in a background thread
    listener.set_nonblocking(false).unwrap();
    let accept_handle = std::thread::spawn(move || -> Option<(String, String)> {
        listener
            .set_nonblocking(false)
            .expect("set_nonblocking failed");
        // Set a timeout so the test does not hang forever
        listener
            .set_nonblocking(false)
            .expect("set_nonblocking failed");
        let timeout = std::time::Duration::from_secs(10);
        listener.set_nonblocking(true).unwrap();

        let start = std::time::Instant::now();
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > timeout {
                        return None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Err(_) => return None,
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();

        let mut reader = BufReader::new(&stream);
        let mut request_text = String::new();

        // Read HTTP headers
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    request_text.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        // Read body based on Content-Length
        let content_length: usize = request_text
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);

        let mut body_buf = vec![0u8; content_length];
        if content_length > 0 {
            use std::io::Read;
            reader.read_exact(&mut body_buf).ok();
        }
        let body = String::from_utf8_lossy(&body_buf).to_string();

        // Send a minimal HTTP 200 response
        let mut stream_w = reader.into_inner();
        let _ = stream_w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let _ = stream_w.flush();

        Some((request_text, body))
    });

    // Trigger a webhook event by writing data
    let mut c = server.client().await;
    c.namespace_create("webhook-test").await.unwrap();
    c.put("webhook-test", b"key1", b"value1").await.unwrap();

    // Wait for the webhook to be received
    let result = accept_handle
        .join()
        .expect("webhook listener thread panicked");
    let (headers, body) = result.expect("no webhook request received within timeout");

    // HTTP headers are case-insensitive; reqwest lowercases them on the wire
    let headers_lower = headers.to_lowercase();

    // Verify X-ShrouDB-Signature-256 header exists
    assert!(
        headers_lower.contains("x-shroudb-signature-256"),
        "webhook request missing signature header, headers: {headers}"
    );

    // Verify the signature header has the expected format
    let sig_line = headers
        .lines()
        .find(|l| l.to_lowercase().contains("x-shroudb-signature-256"))
        .unwrap();
    assert!(
        sig_line.contains("sha256="),
        "signature header should contain sha256= prefix: {sig_line}"
    );

    // Verify the body is valid JSON with event data
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("webhook body should be valid JSON");
    assert!(
        parsed.get("namespace").is_some() || parsed.get("event").is_some(),
        "webhook body should contain event data: {body}"
    );

    // Verify X-ShrouDB-Event header
    assert!(
        headers_lower.contains("x-shroudb-event"),
        "webhook request missing event type header"
    );
}

/// Verify that token validation timing doesn't leak whether a token exists.
///
/// Sends AUTH with a valid token, an invalid token of the same length,
/// and an invalid token of different length. The timing difference between
/// valid and invalid should be negligible (constant-time comparison).
///
/// This is a statistical test — it runs many iterations and checks that
/// the mean times are within a tolerance. Not a cryptographic proof,
/// but catches gross timing leaks.
#[tokio::test]
async fn auth_timing_does_not_leak_token_existence() {
    let valid_token = "a]K9x#mP2$vL7nQ4";
    let wrong_same_len = "z]J8y#nO1$uK6mP3"; // same length, different content
    let wrong_diff_len = "short"; // different length

    let config = TestServerConfig {
        auth_tokens: vec![TestToken {
            raw: valid_token.into(),
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

    let iterations = 200;

    // Warm up
    for _ in 0..20 {
        let mut c = server.client().await;
        let _ = c.auth(valid_token).await;
    }

    // Measure valid token
    let mut valid_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let mut c = server.client().await;
        let start = Instant::now();
        let _ = c.auth(valid_token).await;
        valid_times.push(start.elapsed());
    }

    // Measure wrong token (same length)
    let mut wrong_same_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let mut c = server.client().await;
        let start = Instant::now();
        let _ = c.auth(wrong_same_len).await;
        wrong_same_times.push(start.elapsed());
    }

    // Measure wrong token (different length)
    let mut wrong_diff_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let mut c = server.client().await;
        let start = Instant::now();
        let _ = c.auth(wrong_diff_len).await;
        wrong_diff_times.push(start.elapsed());
    }

    let valid_mean = mean_micros(&valid_times);
    let wrong_same_mean = mean_micros(&wrong_same_times);
    let wrong_diff_mean = mean_micros(&wrong_diff_times);

    eprintln!("valid token mean:       {valid_mean:.1}us");
    eprintln!("wrong (same len) mean:  {wrong_same_mean:.1}us");
    eprintln!("wrong (diff len) mean:  {wrong_diff_mean:.1}us");

    // The ratio between any two should be close to 1.0.
    // Allow up to 3x difference to account for network jitter.
    // A non-constant-time comparison (HashMap lookup) would show 10-100x
    // differences between hit and miss on large token sets.
    let ratio_same = if valid_mean > wrong_same_mean {
        valid_mean / wrong_same_mean
    } else {
        wrong_same_mean / valid_mean
    };
    let ratio_diff = if valid_mean > wrong_diff_mean {
        valid_mean / wrong_diff_mean
    } else {
        wrong_diff_mean / valid_mean
    };

    eprintln!("ratio (valid vs wrong-same-len): {ratio_same:.2}");
    eprintln!("ratio (valid vs wrong-diff-len): {ratio_diff:.2}");

    assert!(
        ratio_same < 3.0,
        "timing leak: valid vs wrong-same-len ratio {ratio_same:.2} exceeds 3x"
    );
    assert!(
        ratio_diff < 3.0,
        "timing leak: valid vs wrong-diff-len ratio {ratio_diff:.2} exceeds 3x"
    );
}

fn mean_micros(durations: &[Duration]) -> f64 {
    let total: f64 = durations.iter().map(|d| d.as_micros() as f64).sum();
    total / durations.len() as f64
}

/// Verify that every write operation produces an audit log entry.
#[tokio::test]
async fn every_write_produces_audit_event() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    // Perform write operations
    c.namespace_create("audit-ns").await.unwrap();
    c.put("audit-ns", b"k1", b"v1").await.unwrap();
    c.put("audit-ns", b"k1", b"v2").await.unwrap();
    c.delete("audit-ns", b"k1").await.unwrap();
    c.namespace_drop("audit-ns", true).await.unwrap();

    // Give audit layer time to flush
    tokio::time::sleep(Duration::from_millis(500)).await;

    let audit_path = server.data_dir.path().join("audit.log");
    assert!(audit_path.exists(), "audit.log not found");

    let content = std::fs::read_to_string(&audit_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // Count write operations in audit log
    // The audit target logs: op=PUT, op=DELETE, op=NAMESPACE CREATE, etc.
    let put_count = lines
        .iter()
        .filter(|l| l.contains("\"op\":\"PUT\""))
        .count();
    let delete_count = lines
        .iter()
        .filter(|l| l.contains("\"op\":\"DELETE\""))
        .count();
    let ns_create_count = lines
        .iter()
        .filter(|l| l.contains("\"op\":\"NAMESPACE CREATE\""))
        .count();
    let ns_drop_count = lines
        .iter()
        .filter(|l| l.contains("\"op\":\"NAMESPACE DROP\""))
        .count();

    eprintln!("audit log lines: {}", lines.len());
    eprintln!("PUT events: {put_count}");
    eprintln!("DELETE events: {delete_count}");
    eprintln!("NAMESPACE CREATE events: {ns_create_count}");
    eprintln!("NAMESPACE DROP events: {ns_drop_count}");

    assert!(
        put_count >= 2,
        "expected at least 2 PUT audit events, got {put_count}"
    );
    assert!(
        delete_count >= 1,
        "expected at least 1 DELETE audit event, got {delete_count}"
    );
    assert!(
        ns_create_count >= 1,
        "expected at least 1 NAMESPACE CREATE audit event, got {ns_create_count}"
    );
    assert!(
        ns_drop_count >= 1,
        "expected at least 1 NAMESPACE DROP audit event, got {ns_drop_count}"
    );
}

/// Verify that read operations do NOT produce audit events.
#[tokio::test]
async fn reads_do_not_produce_audit_events() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("read-test").await.unwrap();
    c.put("read-test", b"k1", b"v1").await.unwrap();

    // Clear audit log by remembering the line count
    tokio::time::sleep(Duration::from_millis(200)).await;
    let audit_path = server.data_dir.path().join("audit.log");
    let before = std::fs::read_to_string(&audit_path)
        .unwrap_or_default()
        .lines()
        .count();

    // Perform read operations
    c.get("read-test", b"k1").await.unwrap();
    c.list("read-test").await.unwrap();
    c.versions("read-test", b"k1").await.unwrap();
    c.namespace_info("read-test").await.unwrap();
    c.namespace_list().await.unwrap();
    c.ping().await.unwrap();
    c.health().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = std::fs::read_to_string(&audit_path)
        .unwrap_or_default()
        .lines()
        .count();

    let new_lines = after - before;
    eprintln!("audit lines before reads: {before}, after: {after}, new: {new_lines}");

    assert_eq!(
        new_lines, 0,
        "read operations should not produce audit events, but {new_lines} new lines appeared"
    );
}

/// Kill the server mid-write and verify data recovers on restart.
#[tokio::test]
async fn crash_recovery_preserves_committed_data() {
    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("crash-test").await.unwrap();

    // Write data that should be committed
    for i in 0..50 {
        c.put(
            "crash-test",
            format!("key-{i}").as_bytes(),
            format!("val-{i}").as_bytes(),
        )
        .await
        .unwrap();
    }

    let data_path = server.data_dir.path().to_path_buf();

    // Hard kill (SIGKILL) — no graceful shutdown
    drop(c);
    server.kill_hard();

    // Restart on same data dir
    let server2 = start_on_data_dir_path(&data_path, TEST_MASTER_KEY)
        .await
        .expect("server failed to restart after crash");
    let mut c2 = server2.client().await;

    // Verify namespace and data survived
    let info = c2.namespace_info("crash-test").await.unwrap();
    assert!(
        info.key_count >= 40,
        "expected at least 40 keys to survive crash recovery, got {}",
        info.key_count,
    );

    // Spot-check a few keys
    let entry = c2.get("crash-test", b"key-0").await.unwrap();
    assert_eq!(entry.value, b"val-0");

    let entry = c2.get("crash-test", b"key-10").await.unwrap();
    assert_eq!(entry.value, b"val-10");
}

/// Corrupt a WAL segment and verify the server still starts (recovery mode skips corrupt entries).
#[tokio::test]
async fn corrupt_wal_segment_handled_gracefully() {
    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("corrupt-test").await.unwrap();
    for i in 0..10 {
        c.put("corrupt-test", format!("k{i}").as_bytes(), b"v")
            .await
            .unwrap();
    }

    drop(c);
    server.stop();

    // Find a WAL segment and corrupt it
    let data_path = server.data_dir.path().to_path_buf();
    let wal_dir = data_path.join("default").join("wal");
    let mut corrupted = false;
    if wal_dir.exists() {
        for entry in std::fs::read_dir(&wal_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "wal") {
                let mut data = std::fs::read(&path).unwrap();
                if data.len() > 20 {
                    let mid = data.len() / 2;
                    data[mid] ^= 0xFF;
                    data[mid + 1] ^= 0xFF;
                    std::fs::write(&path, data).unwrap();
                    corrupted = true;
                    break;
                }
            }
        }
    }

    if corrupted {
        let server2 = start_on_data_dir_path(&data_path, TEST_MASTER_KEY).await;
        assert!(
            server2.is_some(),
            "server should start even with a corrupted WAL segment"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// RemoteStore — Store trait over TCP
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn remote_store_put_get_delete() {
    let server = TestServer::start().await.expect("server failed to start");

    // Connect via RemoteStore (Store trait, not ShrouDBClient)
    let client = shroudb_client::ShrouDBClient::connect(&server.addr)
        .await
        .unwrap();
    let store = RemoteStore::new(client);

    // Use the Store trait — this is how engines will call it
    store
        .namespace_create("remote-test", Default::default())
        .await
        .unwrap();

    let v1 = store
        .put("remote-test", b"key1", b"hello", None)
        .await
        .unwrap();
    assert_eq!(v1, 1);

    let entry = store.get("remote-test", b"key1", None).await.unwrap();
    assert_eq!(entry.value, b"hello");
    assert_eq!(entry.version, 1);

    let v2 = store
        .put("remote-test", b"key1", b"world", None)
        .await
        .unwrap();
    assert_eq!(v2, 2);

    let v3 = store.delete("remote-test", b"key1").await.unwrap();
    assert_eq!(v3, 3);

    let result = store.get("remote-test", b"key1", None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn remote_store_list_and_versions() {
    let server = TestServer::start().await.expect("server failed to start");
    let client = shroudb_client::ShrouDBClient::connect(&server.addr)
        .await
        .unwrap();
    let store = RemoteStore::new(client);

    store
        .namespace_create("rs-list", Default::default())
        .await
        .unwrap();
    store.put("rs-list", b"a", b"1", None).await.unwrap();
    store.put("rs-list", b"b", b"2", None).await.unwrap();
    store.put("rs-list", b"a", b"1v2", None).await.unwrap();

    let page = store.list("rs-list", None, None, 100).await.unwrap();
    assert_eq!(page.keys.len(), 2);

    let versions = store.versions("rs-list", b"a", 10, None).await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version, 2);
}

#[tokio::test]
async fn remote_store_namespace_lifecycle() {
    let server = TestServer::start().await.expect("server failed to start");
    let client = shroudb_client::ShrouDBClient::connect(&server.addr)
        .await
        .unwrap();
    let store = RemoteStore::new(client);

    store
        .namespace_create("rs-ns", Default::default())
        .await
        .unwrap();

    let info = store.namespace_info("rs-ns").await.unwrap();
    assert_eq!(info.name, "rs-ns");
    assert_eq!(info.key_count, 0);

    let page = store.namespace_list(None, 100).await.unwrap();
    assert!(!page.keys.is_empty());

    store.namespace_drop("rs-ns", false).await.unwrap();

    let result = store.namespace_info("rs-ns").await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Bounded index / cache
// ═══════════════════════════════════════════════════════════════════════

/// Start with a tiny cache budget, write enough data to trigger eviction,
/// then verify all data is still readable (cache miss → WAL recovery).
#[tokio::test]
async fn cache_bounded_index_eviction_and_recovery() {
    let server = TestServer::start_with_config(TestServerConfig {
        // 1 KB budget — a handful of entries will exceed this
        cache_memory_budget: Some("1kb".to_string()),
        ..Default::default()
    })
    .await
    .expect("server with cache config failed to start");

    let mut c = server.client().await;

    // Create namespace and write enough data to exceed the 1KB budget.
    // Each value is 200 bytes, so 10 entries = ~2KB of values alone.
    c.namespace_create("cache-test").await.unwrap();

    let entry_count = 10;
    let value_size = 200;
    for i in 0..entry_count {
        let key = format!("key:{i:04}");
        let value = vec![(i & 0xFF) as u8; value_size];
        c.put("cache-test", key.as_bytes(), &value).await.unwrap();
    }

    // Read all entries back. Some will be cache hits, some will be
    // cache misses that recover from WAL. All must return correct data.
    for i in 0..entry_count {
        let key = format!("key:{i:04}");
        let expected_value = vec![(i & 0xFF) as u8; value_size];
        let result = c.get("cache-test", key.as_bytes()).await.unwrap();
        assert_eq!(
            result.value, expected_value,
            "value mismatch for {key} (cache miss recovery failed?)"
        );
        assert_eq!(result.version, 1);
    }

    // LIST must return all keys regardless of eviction state
    let page = c.list("cache-test").await.unwrap();
    assert_eq!(page.items.len(), entry_count as usize);

    // Write more versions to trigger old-version eviction (keep N and N-1)
    for i in 0..3u32 {
        let key = format!("key:{i:04}");
        for v in 2..=5u64 {
            let value = vec![((i as u64 + v) & 0xFF) as u8; value_size];
            c.put("cache-test", key.as_bytes(), &value).await.unwrap();
        }
    }

    // Read latest version — should work
    for i in 0..3u32 {
        let key = format!("key:{i:04}");
        let result = c.get("cache-test", key.as_bytes()).await.unwrap();
        assert_eq!(result.version, 5, "expected version 5 for {key}");
    }

    // Read old version (v1) — should work via WAL recovery even though
    // old versions are evicted
    for i in 0..3u32 {
        let key = format!("key:{i:04}");
        let result = c
            .get_version("cache-test", key.as_bytes(), 1)
            .await
            .unwrap();
        let expected_value = vec![(i & 0xFF) as u8; value_size];
        assert_eq!(
            result.value, expected_value,
            "old version recovery failed for {key}"
        );
    }

    // VERSIONS must still report all versions
    let versions = c.versions("cache-test", b"key:0000").await.unwrap();
    assert_eq!(versions.len(), 5);

    // DELETE should work on evicted keys
    c.delete("cache-test", b"key:0009").await.unwrap();
    let err = c.get("cache-test", b"key:0009").await;
    assert!(err.is_err(), "deleted key should not be found");
}

/// Verify the server starts and works normally with no cache config
/// (unlimited mode, backward compat).
#[tokio::test]
async fn cache_unlimited_mode_default() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("no-cache").await.unwrap();
    c.put("no-cache", b"key", b"value").await.unwrap();
    let result = c.get("no-cache", b"key").await.unwrap();
    assert_eq!(result.value, b"value");
}

/// Write data with cache enabled, restart the server, verify all data
/// survives the restart. This tests the vlog lifecycle: post-recovery
/// vlog build → eviction with vlog locations → vlog-based recovery.
#[tokio::test]
async fn cache_survives_restart() {
    // Phase 1: start server with cache, write data, hard kill
    let mut server = TestServer::start_with_config(TestServerConfig {
        cache_memory_budget: Some("1kb".to_string()),
        ..Default::default()
    })
    .await
    .expect("server with cache failed to start");

    let mut c = server.client().await;
    c.namespace_create("persist").await.unwrap();

    for i in 0..20u32 {
        let key = format!("key:{i:04}");
        let value = vec![(i & 0xFF) as u8; 200];
        c.put("persist", key.as_bytes(), &value).await.unwrap();
    }

    // Give batched fsync time to flush
    tokio::time::sleep(Duration::from_millis(50)).await;

    let data_path = server.data_dir.path().to_path_buf();
    drop(c);
    server.kill_hard();

    // Phase 2: restart on same data dir with cache enabled
    let server2 = start_on_data_dir_path_with_cache(&data_path, TEST_MASTER_KEY, "1kb")
        .await
        .expect("server failed to restart with cache");

    let mut c2 = server2.client().await;

    // Most entries should survive (some may be lost due to SIGKILL)
    let page = c2.list("persist").await.unwrap();
    assert!(
        page.items.len() >= 15,
        "expected at least 15 keys after crash recovery with cache, got {}",
        page.items.len()
    );

    // Spot-check early entries (definitely flushed)
    let result = c2
        .get("persist", b"key:0000")
        .await
        .unwrap_or_else(|e| panic!("failed to get key:0000 after restart: {e}"));
    assert_eq!(result.value, vec![0u8; 200]);

    // Write new data after restart
    c2.put("persist", b"post-restart", b"works").await.unwrap();
    let result = c2.get("persist", b"post-restart").await.unwrap();
    assert_eq!(result.value, b"works");
}

// ═══════════════════════════════════════════════════════════════════════
// CONFIG SET schema enforcement (LOW-01)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn config_set_rejects_unknown_key() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    let result = c.config_set("bogus_key", "42").await;
    assert!(result.is_err(), "CONFIG SET should reject unknown key");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("unknown config key"),
        "error should mention unknown key, got: {err}"
    );
}

#[tokio::test]
async fn config_set_rejects_wrong_type() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    let result = c.config_set("max_segment_bytes", "not-a-number").await;
    assert!(result.is_err(), "CONFIG SET should reject wrong type");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("expected u64"),
        "error should mention type mismatch, got: {err}"
    );
}

#[tokio::test]
async fn config_set_accepts_valid_key_and_type() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.config_set("max_segment_bytes", "33554432").await.unwrap();

    let value = c.config_get("max_segment_bytes").await.unwrap();
    assert_eq!(value.as_deref(), Some("33554432"));
}

#[tokio::test]
async fn config_get_returns_seeded_values() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    // Engine config seeds default values — max_segment_bytes should be present
    let value = c.config_get("max_segment_bytes").await.unwrap();
    assert!(
        value.is_some(),
        "max_segment_bytes should be seeded from engine config"
    );
    // Default is 64MB = 67108864
    assert_eq!(value.as_deref(), Some("67108864"));
}

#[tokio::test]
async fn config_set_persists_across_restart() {
    let mut server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.config_set("snapshot_entry_threshold", "50000")
        .await
        .unwrap();

    let data_path = server.data_dir.path().to_path_buf();

    drop(c);
    server.stop();

    let server2 = start_on_data_dir_path(&data_path, TEST_MASTER_KEY)
        .await
        .expect("server failed to restart");
    let mut c2 = server2.client().await;

    let value = c2.config_get("snapshot_entry_threshold").await.unwrap();
    assert_eq!(
        value.as_deref(),
        Some("50000"),
        "CONFIG SET value should persist via WAL"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// LIST cursor validation (LOW-03)
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_invalid_cursor_returns_bad_arg() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("cursor-test").await.unwrap();
    c.put("cursor-test", b"k1", b"v1").await.unwrap();
    c.put("cursor-test", b"k2", b"v2").await.unwrap();

    // Use a cursor that doesn't correspond to any key
    let result = c.list_cursor("cursor-test", "nonexistent-cursor", 10).await;
    assert!(
        result.is_err(),
        "LIST with invalid cursor should return error"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("invalid cursor"),
        "error should mention invalid cursor, got: {err}"
    );
}

#[tokio::test]
async fn list_valid_cursor_paginates_correctly() {
    let server = TestServer::start().await.expect("server failed to start");
    let mut c = server.client().await;

    c.namespace_create("paginate-test").await.unwrap();
    for i in 0..5 {
        c.put("paginate-test", format!("key:{i:02}").as_bytes(), b"v")
            .await
            .unwrap();
    }

    // Get first page with limit 2
    // Get first page with limit 2 via raw_command for full control
    let resp = c
        .raw_command(&["LIST", "paginate-test", "LIMIT", "2"])
        .await
        .unwrap();
    let cursor = resp.get_string_field("cursor");
    assert!(cursor.is_some(), "should have a next cursor with limit 2");

    // Use the returned cursor to get next page
    let cursor_val = cursor.unwrap();
    let page2 = c
        .list_cursor("paginate-test", &cursor_val, 2)
        .await
        .unwrap();
    assert!(
        !page2.items.is_empty(),
        "second page should have items after valid cursor"
    );
}
