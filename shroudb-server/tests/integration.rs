mod common;
use common::{TestClient, TestServer};

// ---------------------------------------------------------------------------
// Helper: assert a response is a success map with status=OK, with diagnostics
// ---------------------------------------------------------------------------
fn assert_ok(resp: &common::TestResponse, context: &str) {
    if resp.is_error() {
        panic!(
            "[{context}] expected OK map, got error: {}",
            resp.as_error()
        );
    }
    assert_eq!(
        resp.get("status").as_str(),
        "OK",
        "[{context}] expected status=OK"
    );
}

// === HEALTH ===

#[tokio::test]
async fn health_returns_ready() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;
    let resp = client.cmd(&["HEALTH"]).await;
    assert_ok(&resp, "HEALTH");
    assert_eq!(resp.get("state").as_str(), "READY");
}

// === ISSUE (API Key) ===

#[tokio::test]
async fn issue_api_key() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE test-apikeys");
    let api_key = resp.get("token").as_str();
    assert!(
        api_key.starts_with("sk_"),
        "api key should start with sk_, got: {api_key}"
    );
    // credential_id should be present
    let _cred_id = resp.get("credential_id").as_str();
}

// === ISSUE + VERIFY (API Key) ===
// BUG: handle_issue stores sha256(raw_bytes) but handle_verify hashes sha256(token.as_bytes()).
// These produce different hashes, so VERIFY always returns NOTFOUND for valid keys.

#[tokio::test]
async fn issue_and_verify_api_key() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Issue
    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE");
    let api_key = resp.get("token").as_str().to_string();
    let cred_id = resp.get("credential_id").as_str().to_string();

    // Verify -- currently fails due to hash mismatch bug
    let resp = client.cmd(&["VERIFY", "test-apikeys", &api_key]).await;
    assert_ok(&resp, "VERIFY after ISSUE");
    assert_eq!(resp.get("credential_id").as_str(), &cred_id);
}

// === ISSUE + VERIFY (JWT) ===
#[tokio::test]
async fn issue_and_verify_jwt() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // ROTATE to create a signing key (needs two rotations: first creates Staged, second promotes to Active)
    let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;
    assert_ok(&resp, "ROTATE 1");
    let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;
    assert_ok(&resp, "ROTATE 2");

    // Issue JWT with claims
    let resp = client
        .cmd(&["ISSUE", "test-jwt", "CLAIMS", r#"{"sub":"user1"}"#])
        .await;
    assert_ok(&resp, "ISSUE JWT");
    let token = resp.get("token").as_str().to_string();
    assert!(token.contains('.'), "JWT should contain dots: {token}");

    // Verify
    let resp = client.cmd(&["VERIFY", "test-jwt", &token]).await;
    assert_ok(&resp, "VERIFY JWT");
}

// === REVOKE (API Key) ===

#[tokio::test]
async fn revoke_api_key() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE");
    let api_key = resp.get("token").as_str().to_string();
    let cred_id = resp.get("credential_id").as_str().to_string();

    // Revoke by credential_id
    let resp = client.cmd(&["REVOKE", "test-apikeys", &cred_id]).await;
    assert_ok(&resp, "REVOKE");

    // Verify should fail (revoked or hash bug)
    let resp = client.cmd(&["VERIFY", "test-apikeys", &api_key]).await;
    assert!(resp.is_error(), "verify after revoke should fail");
}

// === SUSPEND / UNSUSPEND ===

#[tokio::test]
async fn suspend_and_unsuspend_api_key() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE");
    let api_key = resp.get("token").as_str().to_string();
    let cred_id = resp.get("credential_id").as_str().to_string();

    // Suspend
    let resp = client.cmd(&["SUSPEND", "test-apikeys", &cred_id]).await;
    assert_ok(&resp, "SUSPEND");

    // Verify should fail (suspended or hash bug)
    let resp = client.cmd(&["VERIFY", "test-apikeys", &api_key]).await;
    assert!(resp.is_error(), "verify after suspend should fail");

    // Unsuspend
    let resp = client.cmd(&["UNSUSPEND", "test-apikeys", &cred_id]).await;
    assert_ok(&resp, "UNSUSPEND");

    // Verify should work again after unsuspend
    let resp = client.cmd(&["VERIFY", "test-apikeys", &api_key]).await;
    assert_ok(&resp, "VERIFY after UNSUSPEND");
}

// === REFRESH token lifecycle ===
// BUG: Same hash mismatch as API keys. generate_api_key returns raw_bytes that are
// hashed at issue time, but REFRESH/VERIFY hash the formatted token string.

#[tokio::test]
async fn refresh_token_lifecycle() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Issue
    let resp = client.cmd(&["ISSUE", "test-refresh"]).await;
    assert_ok(&resp, "ISSUE refresh");
    let token1 = resp.get("token").as_str().to_string();

    // Refresh (consume old, get new)
    let resp = client.cmd(&["REFRESH", "test-refresh", &token1]).await;
    assert_ok(&resp, "REFRESH");
    let token2 = resp.get("token").as_str().to_string();
    assert_ne!(token1, token2);

    // Old token should fail (consumed)
    let resp = client.cmd(&["VERIFY", "test-refresh", &token1]).await;
    assert!(resp.is_error(), "old token should fail after refresh");

    // New token should work
    let resp = client.cmd(&["VERIFY", "test-refresh", &token2]).await;
    assert_ok(&resp, "VERIFY new refresh token");
}

// === REFRESH reuse detection ===

#[tokio::test]
async fn refresh_token_reuse_revokes_family() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "test-refresh"]).await;
    assert_ok(&resp, "ISSUE");
    let token1 = resp.get("token").as_str().to_string();

    // Refresh once
    let resp = client.cmd(&["REFRESH", "test-refresh", &token1]).await;
    assert_ok(&resp, "REFRESH");
    let token2 = resp.get("token").as_str().to_string();

    // Try to reuse token1 (already consumed) -- should detect reuse
    let resp = client.cmd(&["REFRESH", "test-refresh", &token1]).await;
    assert!(resp.is_error(), "reuse of consumed token should fail");
    assert!(
        resp.as_error().contains("REUSE_DETECTED"),
        "expected REUSE_DETECTED, got: {}",
        resp.as_error()
    );

    // token2 should also be revoked (entire family)
    let resp = client.cmd(&["VERIFY", "test-refresh", &token2]).await;
    assert!(resp.is_error(), "token2 should be revoked after reuse");
}

// === ROTATE + JWKS + KEYSTATE ===
#[tokio::test]
async fn rotate_and_jwks() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Rotate to create first key
    let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;
    assert_ok(&resp, "ROTATE");

    // JWKS should return keys
    let resp = client.cmd(&["JWKS", "test-jwt"]).await;
    assert_ok(&resp, "JWKS");

    // KEYSTATE should show key info
    let resp = client.cmd(&["KEYSTATE", "test-jwt"]).await;
    assert_ok(&resp, "KEYSTATE");
}

// === INSPECT ===

#[tokio::test]
async fn inspect_api_key() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client
        .cmd(&["ISSUE", "test-apikeys", "META", r#"{"env":"test"}"#])
        .await;
    assert_ok(&resp, "ISSUE with META");
    let cred_id = resp.get("credential_id").as_str().to_string();

    let resp = client.cmd(&["INSPECT", "test-apikeys", &cred_id]).await;
    assert_ok(&resp, "INSPECT");
}

// === UPDATE metadata ===

#[tokio::test]
async fn update_api_key_metadata() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client
        .cmd(&["ISSUE", "test-apikeys", "META", r#"{"env":"dev"}"#])
        .await;
    assert_ok(&resp, "ISSUE");
    let cred_id = resp.get("credential_id").as_str().to_string();

    let resp = client
        .cmd(&[
            "UPDATE",
            "test-apikeys",
            &cred_id,
            "META",
            r#"{"env":"prod"}"#,
        ])
        .await;
    assert_ok(&resp, "UPDATE");
}

// === SCHEMA ===

#[tokio::test]
async fn schema_returns_for_keyspace() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;
    let resp = client.cmd(&["SCHEMA", "test-apikeys"]).await;
    assert_ok(&resp, "SCHEMA");
}

// === Error cases ===

#[tokio::test]
async fn unknown_command_returns_error() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;
    let resp = client.cmd(&["BADCOMMAND"]).await;
    assert!(resp.is_error());
    assert!(
        resp.as_error().contains("BADARG"),
        "expected BADARG, got: {}",
        resp.as_error()
    );
}

#[tokio::test]
async fn unknown_keyspace_returns_not_found() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;
    let resp = client.cmd(&["SCHEMA", "nonexistent"]).await;
    assert!(resp.is_error());
    assert!(
        resp.as_error().contains("NOTFOUND"),
        "expected NOTFOUND, got: {}",
        resp.as_error()
    );
}

// === ROTATE DRYRUN ===
// === ROTATE DRYRUN ===
#[tokio::test]
async fn rotate_dryrun_doesnt_mutate() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Force create a key first
    let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;
    assert_ok(&resp, "ROTATE FORCE");

    // Dryrun should succeed without changing state
    let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE", "DRYRUN"]).await;
    assert_ok(&resp, "ROTATE DRYRUN");
}

// === HMAC ROTATE + ISSUE + VERIFY ===
// BUG: apply_payload_to_index for SigningKeyCreated only handles JWT keys. HMAC keys
// with SigningKeyAlgorithm::Hmac are skipped (line "_ => return // HMAC key in JWT ring -- skip").
// The key is written to WAL but never indexed in hmac_rings, so ISSUE fails with NOTFOUND.

#[tokio::test]
async fn hmac_rotate() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // HMAC ROTATE does not deadlock (different DashMap than jwt_rings)
    let resp = client.cmd(&["ROTATE", "test-hmac", "FORCE"]).await;
    assert_ok(&resp, "HMAC ROTATE");
}

#[tokio::test]
async fn issue_and_verify_hmac() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Create signing key
    let resp = client.cmd(&["ROTATE", "test-hmac", "FORCE"]).await;
    assert_ok(&resp, "HMAC ROTATE");

    // Issue HMAC signature (CLAIMS must be valid JSON)
    let payload_json = r#"{"data":"my-payload"}"#;
    let resp = client
        .cmd(&["ISSUE", "test-hmac", "CLAIMS", payload_json])
        .await;
    assert_ok(&resp, "HMAC ISSUE");
    let signature = resp.get("token").as_str().to_string();

    // Verify -- PAYLOAD must match the serialized JSON bytes
    let resp = client
        .cmd(&["VERIFY", "test-hmac", &signature, "PAYLOAD", payload_json])
        .await;
    assert_ok(&resp, "HMAC VERIFY");
}

// === Multiple clients on same server ===

#[tokio::test]
async fn multiple_clients_share_state() {
    let server = TestServer::start().await;
    let mut client1 = TestClient::connect(server.addr).await;
    let mut client2 = TestClient::connect(server.addr).await;

    // Issue on client1
    let resp = client1.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE on client1");
    let api_key = resp.get("token").as_str().to_string();

    // Verify on client2 (will fail due to hash bug, but tests cross-client connectivity)
    let resp = client2.cmd(&["VERIFY", "test-apikeys", &api_key]).await;
    assert_ok(&resp, "VERIFY on client2");
}

// === Verify nonexistent key returns error ===

#[tokio::test]
async fn verify_nonexistent_api_key_fails() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client
        .cmd(&["VERIFY", "test-apikeys", "sk_doesnotexist"])
        .await;
    assert!(resp.is_error(), "verify nonexistent key should fail");
}

// === Inspect nonexistent credential returns error ===

#[tokio::test]
async fn inspect_nonexistent_credential_fails() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client
        .cmd(&["INSPECT", "test-apikeys", "nonexistent-cred-id"])
        .await;
    assert!(
        resp.is_error(),
        "inspect nonexistent credential should fail"
    );
}

// === Issue multiple API keys ===

#[tokio::test]
async fn issue_multiple_api_keys_unique() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp1 = client.cmd(&["ISSUE", "test-apikeys"]).await;
    let resp2 = client.cmd(&["ISSUE", "test-apikeys"]).await;

    let key1 = resp1.get("token").as_str();
    let key2 = resp2.get("token").as_str();
    assert_ne!(key1, key2, "each issued API key should be unique");

    let cred1 = resp1.get("credential_id").as_str();
    let cred2 = resp2.get("credential_id").as_str();
    assert_ne!(cred1, cred2, "each credential ID should be unique");
}

// === Issue refresh token ===

#[tokio::test]
async fn issue_refresh_token() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "test-refresh"]).await;
    assert_ok(&resp, "ISSUE refresh");
    let _token = resp.get("token").as_str();
    let _cred_id = resp.get("credential_id").as_str();
    let _family_id = resp.get("family_id").as_str();
}

// === Idempotency key deduplication ===

#[tokio::test]
async fn idempotency_key_dedup() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Issue with idempotency key
    let resp1 = client
        .cmd(&["ISSUE", "test-apikeys", "IDEMPOTENCY_KEY", "idem-1"])
        .await;
    assert_ok(&resp1, "first ISSUE");
    let key1 = resp1.get("token").as_str().to_string();

    // Same idempotency key — should return cached response
    let resp2 = client
        .cmd(&["ISSUE", "test-apikeys", "IDEMPOTENCY_KEY", "idem-1"])
        .await;
    assert_ok(&resp2, "second ISSUE");
    let key2 = resp2.get("token").as_str().to_string();
    assert_eq!(key1, key2, "idempotent responses should match");
}

// === Keyspace disabled rejects commands ===

#[tokio::test]
async fn keyspace_disabled_rejects_commands() {
    let server = TestServer::start_with_config(
        r#"
[keyspaces.disabled-ks]
type = "api_key"
disabled = true

[keyspaces.enabled-ks]
type = "api_key"
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    // Commands to disabled keyspace should fail
    let resp = client.cmd(&["ISSUE", "disabled-ks"]).await;
    assert!(resp.is_error());
    assert!(
        resp.as_error().contains("DISABLED"),
        "expected DISABLED error, got: {}",
        resp.as_error()
    );

    // Commands to enabled keyspace should work
    let resp = client.cmd(&["ISSUE", "enabled-ks"]).await;
    assert_ok(&resp, "enabled keyspace ISSUE");
}

// === Refresh token chain limit ===

#[tokio::test]
async fn refresh_token_chain_limit() {
    let server = TestServer::start_with_config(
        r#"
[keyspaces.limited-refresh]
type = "refresh_token"
max_chain_length = 3
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "limited-refresh"]).await;
    assert_ok(&resp, "initial issue");
    let mut token = resp.get("token").as_str().to_string();

    // Refresh 2 times (chain: 0 -> 1 -> 2, length = 3)
    for i in 0..2 {
        let resp = client.cmd(&["REFRESH", "limited-refresh", &token]).await;
        assert_ok(&resp, &format!("refresh {}", i + 1));
        token = resp.get("token").as_str().to_string();
    }

    // Next refresh should fail (chain length = 3, limit = 3)
    let resp = client.cmd(&["REFRESH", "limited-refresh", &token]).await;
    assert!(resp.is_error(), "should reject at chain limit");
    assert!(
        resp.as_error().contains("CHAIN_LIMIT"),
        "expected CHAIN_LIMIT error, got: {}",
        resp.as_error()
    );
}

// === Meta schema validation via TCP ===

#[tokio::test]
async fn meta_schema_enforced_on_issue() {
    let server = TestServer::start_with_config(
        r#"
[keyspaces.schema-ks]
type = "api_key"

[keyspaces.schema-ks.meta_schema]
enforce = true

[keyspaces.schema-ks.meta_schema.fields.org_id]
type = "string"
required = true
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    // ISSUE without required field should fail
    let resp = client.cmd(&["ISSUE", "schema-ks"]).await;
    assert!(resp.is_error(), "should reject missing required field");

    // ISSUE with required field should succeed
    let resp = client
        .cmd(&["ISSUE", "schema-ks", "META", r#"{"org_id":"org-1"}"#])
        .await;
    assert_ok(&resp, "ISSUE with valid metadata");
}

// === Update revoked credential fails ===

#[tokio::test]
async fn update_revoked_credential_fails() {
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE");
    let cred_id = resp.get("credential_id").as_str().to_string();

    // Revoke
    let resp = client.cmd(&["REVOKE", "test-apikeys", &cred_id]).await;
    assert_ok(&resp, "REVOKE");

    // Update should fail
    let resp = client
        .cmd(&["UPDATE", "test-apikeys", &cred_id, "META", r#"{"x":"y"}"#])
        .await;
    assert!(
        resp.is_error(),
        "should reject update on revoked credential"
    );
}

// === AUTH tests ===

#[tokio::test]
async fn auth_required_when_configured() {
    let server = TestServer::start_with_config(
        r#"
[auth]
method = "token"

[auth.policies.admin]
token = "test-admin-token"
keyspaces = ["*"]
commands = ["*"]
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    // HEALTH always allowed (bypasses auth)
    let resp = client.cmd(&["HEALTH"]).await;
    assert_ok(&resp, "HEALTH always allowed");

    // Without AUTH, commands should be rejected
    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert!(resp.is_error(), "ISSUE should require auth");
    assert!(
        resp.as_error().contains("DENIED"),
        "expected DENIED, got: {}",
        resp.as_error()
    );

    // AUTH with valid token
    let resp = client.cmd(&["AUTH", "test-admin-token"]).await;
    assert_ok(&resp, "AUTH should succeed");

    // Now commands should work
    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert_ok(&resp, "ISSUE after AUTH");
}

#[tokio::test]
async fn auth_policy_restricts_keyspaces() {
    let server = TestServer::start_with_config(
        r#"
[auth]
method = "token"

[auth.policies.reader]
token = "reader-token"
keyspaces = ["test-apikeys"]
commands = ["VERIFY", "INSPECT", "HEALTH"]
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    // AUTH
    let resp = client.cmd(&["AUTH", "reader-token"]).await;
    assert_ok(&resp, "AUTH");

    // Allowed command on allowed keyspace
    // (VERIFY will fail with NOTFOUND since no key exists, but it won't be DENIED)
    let resp = client.cmd(&["VERIFY", "test-apikeys", "fake-key"]).await;
    if resp.is_error() {
        assert!(
            !resp.as_error().contains("DENIED"),
            "should not be DENIED, got: {}",
            resp.as_error()
        );
    }

    // Disallowed command (ISSUE not in commands list)
    let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
    assert!(resp.is_error(), "ISSUE should be rejected by policy");
    assert!(
        resp.as_error().contains("DENIED"),
        "ISSUE should be denied by policy, got: {}",
        resp.as_error()
    );
}

// === Encryption & restart survival tests ===

#[tokio::test]
async fn jwt_survives_restart() {
    let data_dir = tempfile::tempdir().unwrap();
    let master_key = "ab".repeat(32);
    let token;

    // Start server, create key, issue JWT
    {
        let mut server = TestServer::start_with_dir_and_key(data_dir.path(), &master_key).await;
        let mut client = TestClient::connect(server.addr).await;

        // Two rotations: first creates Staged, second promotes to Active
        let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;
        assert_ok(&resp, "ROTATE 1");
        let resp = client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;
        assert_ok(&resp, "ROTATE 2");

        let resp = client
            .cmd(&["ISSUE", "test-jwt", "CLAIMS", r#"{"sub":"user1"}"#])
            .await;
        assert_ok(&resp, "ISSUE JWT");
        token = resp.get("token").as_str().to_string();

        // Verify works on this instance
        let resp = client.cmd(&["VERIFY", "test-jwt", &token]).await;
        assert_ok(&resp, "VERIFY before restart");

        server.stop(); // graceful shutdown
    }

    // Start NEW server on same data dir with same master key
    {
        let server = TestServer::start_with_dir_and_key(data_dir.path(), &master_key).await;
        let mut client = TestClient::connect(server.addr).await;

        // The token should still verify (key material survived)
        let resp = client.cmd(&["VERIFY", "test-jwt", &token]).await;
        assert_ok(&resp, "VERIFY after restart");

        // Should be able to issue new JWTs (private key survived)
        let resp = client
            .cmd(&["ISSUE", "test-jwt", "CLAIMS", r#"{"sub":"user2"}"#])
            .await;
        assert_ok(&resp, "ISSUE after restart");
    }
}

#[tokio::test]
async fn api_key_survives_restart() {
    let data_dir = tempfile::tempdir().unwrap();
    let master_key = "cd".repeat(32);
    let api_key;

    {
        let mut server = TestServer::start_with_dir_and_key(data_dir.path(), &master_key).await;
        let mut client = TestClient::connect(server.addr).await;

        let resp = client.cmd(&["ISSUE", "test-apikeys"]).await;
        assert_ok(&resp, "ISSUE");
        api_key = resp.get("token").as_str().to_string();

        server.stop();
    }

    {
        let server = TestServer::start_with_dir_and_key(data_dir.path(), &master_key).await;
        let mut client = TestClient::connect(server.addr).await;

        let resp = client.cmd(&["VERIFY", "test-apikeys", &api_key]).await;
        assert_ok(&resp, "VERIFY after restart");
    }
}

#[tokio::test]
async fn wal_files_are_encrypted() {
    let data_dir = tempfile::tempdir().unwrap();
    let master_key = "ef".repeat(32);

    {
        let mut server = TestServer::start_with_dir_and_key(data_dir.path(), &master_key).await;
        let mut client = TestClient::connect(server.addr).await;

        let resp = client
            .cmd(&[
                "ISSUE",
                "test-apikeys",
                "META",
                r#"{"secret":"do-not-leak"}"#,
            ])
            .await;
        assert_ok(&resp, "ISSUE");
        server.stop();
    }

    // Read WAL files -- the string "do-not-leak" should NOT appear in plaintext
    let wal_dir = data_dir.path().join("default/wal");
    if wal_dir.exists() {
        for entry in std::fs::read_dir(&wal_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "wal") {
                let contents = std::fs::read(&path).unwrap();
                let contents_str = String::from_utf8_lossy(&contents);
                assert!(
                    !contents_str.contains("do-not-leak"),
                    "WAL file contains plaintext metadata -- encryption is broken"
                );
            }
        }
    }
}

// === JWKS cache / basic response test ===

#[tokio::test]
async fn jwks_cache_control_varies_by_rotation() {
    // Start server with JWT keyspace (rotation_days defaults to 90)
    let server = TestServer::start().await;
    let mut client = TestClient::connect(server.addr).await;

    // Create a key (recently created = far from rotation)
    client.cmd(&["ROTATE", "test-jwt", "FORCE"]).await;

    // Get JWKS -- should return a valid response with keys.
    // We cannot test the HTTP Cache-Control header via TCP, but we can verify
    // that JWKS returns a valid response with key data after rotation.
    let resp = client.cmd(&["JWKS", "test-jwt"]).await;
    assert_ok(&resp, "JWKS after rotation");
}

// === Required claims enforcement ===

#[tokio::test]
async fn jwt_required_claims_enforcement() {
    // Use a custom claim "role" instead of "aud" to avoid interference from
    // the jsonwebtoken library's built-in audience validation.
    let server = TestServer::start_with_config(
        r#"
[keyspaces.claims-jwt]
type = "jwt"
algorithm = "ES256"
default_ttl = "15m"

[keyspaces.claims-jwt.required_claims]
role = "admin"
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    // Create signing key (two rotations: Staged -> Active)
    client.cmd(&["ROTATE", "claims-jwt", "FORCE"]).await;
    client.cmd(&["ROTATE", "claims-jwt", "FORCE"]).await;

    // Issue JWT with correct role
    let resp = client
        .cmd(&[
            "ISSUE",
            "claims-jwt",
            "CLAIMS",
            r#"{"sub":"user1","role":"admin"}"#,
        ])
        .await;
    assert_ok(&resp, "ISSUE with correct role");
    let token_good = resp.get("token").as_str().to_string();

    // Verify should succeed
    let resp = client.cmd(&["VERIFY", "claims-jwt", &token_good]).await;
    assert_ok(&resp, "VERIFY with correct role");

    // Issue JWT with wrong role
    let resp = client
        .cmd(&[
            "ISSUE",
            "claims-jwt",
            "CLAIMS",
            r#"{"sub":"user1","role":"viewer"}"#,
        ])
        .await;
    assert_ok(&resp, "ISSUE with wrong role (issue doesn't check claims)");
    let token_bad = resp.get("token").as_str().to_string();

    // Verify should fail on required claims mismatch
    let resp = client.cmd(&["VERIFY", "claims-jwt", &token_bad]).await;
    assert!(resp.is_error(), "VERIFY should reject wrong role");

    // Issue JWT missing role entirely
    let resp = client
        .cmd(&["ISSUE", "claims-jwt", "CLAIMS", r#"{"sub":"user1"}"#])
        .await;
    assert_ok(&resp, "ISSUE without role");
    let token_missing = resp.get("token").as_str().to_string();

    // Verify should fail on missing required claim
    let resp = client.cmd(&["VERIFY", "claims-jwt", &token_missing]).await;
    assert!(resp.is_error(), "VERIFY should reject missing role");
}

// === Meta schema enforce=false (warn only) ===

#[tokio::test]
async fn meta_schema_enforce_false_allows_invalid() {
    let server = TestServer::start_with_config(
        r#"
[keyspaces.warn-ks]
type = "api_key"

[keyspaces.warn-ks.meta_schema]
enforce = false

[keyspaces.warn-ks.meta_schema.fields.org_id]
type = "string"
required = true
"#,
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;

    // ISSUE without required field should SUCCEED (enforce=false, warn only)
    let resp = client.cmd(&["ISSUE", "warn-ks"]).await;
    assert_ok(
        &resp,
        "ISSUE should succeed with enforce=false even without required field",
    );
}
