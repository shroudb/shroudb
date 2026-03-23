mod common;

use keyva_client::KeyvaClient;

#[tokio::test]
async fn client_health() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();
    let health = client.health().await.unwrap();
    assert_eq!(health.state, "READY");
}

#[tokio::test]
async fn client_issue_verify_api_key() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client.issue("test-apikeys").execute().await.unwrap();
    assert!(result.api_key.is_some());
    assert!(result.credential_id.is_some());

    let api_key = result.api_key.as_ref().unwrap();
    assert!(api_key.starts_with("sk_"));

    let verify = client.verify("test-apikeys", api_key).await.unwrap();
    assert!(verify.credential_id.is_some());
}

#[tokio::test]
async fn client_issue_with_metadata() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client
        .issue("test-apikeys")
        .metadata(serde_json::json!({"org": "acme"}))
        .execute()
        .await
        .unwrap();
    assert!(result.api_key.is_some());
}

#[tokio::test]
async fn client_revoke() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client.issue("test-apikeys").execute().await.unwrap();
    let cred_id = result.credential_id.as_ref().unwrap();

    client.revoke("test-apikeys", cred_id).await.unwrap();

    let api_key = result.api_key.as_ref().unwrap();
    let verify = client.verify("test-apikeys", api_key).await;
    assert!(verify.is_err(), "verify after revoke should fail");
}

#[tokio::test]
async fn client_suspend_unsuspend() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client.issue("test-apikeys").execute().await.unwrap();
    let cred_id = result.credential_id.as_ref().unwrap();
    let api_key = result.api_key.as_ref().unwrap();

    // Suspend
    client.suspend("test-apikeys", cred_id).await.unwrap();

    // Verify should fail
    let verify = client.verify("test-apikeys", api_key).await;
    assert!(verify.is_err(), "verify after suspend should fail");

    // Unsuspend
    client.unsuspend("test-apikeys", cred_id).await.unwrap();

    // Verify should work again
    let verify = client.verify("test-apikeys", api_key).await.unwrap();
    assert!(verify.credential_id.is_some());
}

#[tokio::test]
async fn client_inspect() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client
        .issue("test-apikeys")
        .metadata(serde_json::json!({"env": "test"}))
        .execute()
        .await
        .unwrap();
    let cred_id = result.credential_id.as_ref().unwrap();

    let inspect = client.inspect("test-apikeys", cred_id).await.unwrap();
    assert_eq!(
        inspect.fields.get("status").and_then(|v| v.as_str()),
        Some("OK")
    );
}

#[tokio::test]
async fn client_schema() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let schema = client.schema("test-apikeys").await.unwrap();
    assert_eq!(
        schema.fields.get("status").and_then(|v| v.as_str()),
        Some("OK")
    );
}

#[tokio::test]
async fn client_jwt_issue_verify() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    // Create signing key (two rotations: Staged -> Active)
    client.rotate_force("test-jwt").await.unwrap();
    client.rotate_force("test-jwt").await.unwrap();

    // Issue JWT
    let result = client
        .issue("test-jwt")
        .claims(serde_json::json!({"sub": "user1"}))
        .execute()
        .await
        .unwrap();
    assert!(result.token.is_some());
    let token = result.token.as_ref().unwrap();
    assert!(token.contains('.'), "JWT should contain dots");

    // Verify -- JWT verify returns claims, not credential_id
    let verify = client.verify("test-jwt", token).await.unwrap();
    assert!(verify.claims.is_some());
}

#[tokio::test]
async fn client_rotate_and_jwks() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    client.rotate_force("test-jwt").await.unwrap();

    let jwks = client.jwks("test-jwt").await.unwrap();
    assert_eq!(
        jwks.fields.get("status").and_then(|v| v.as_str()),
        Some("OK")
    );
}

#[tokio::test]
async fn client_raw_command() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let resp = client.raw_command(&["HEALTH"]).await.unwrap();
    assert!(!resp.is_error());
}

#[tokio::test]
async fn client_refresh_token_lifecycle() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client.issue("test-refresh").execute().await.unwrap();
    let token1 = result.token.as_ref().unwrap().clone();

    // Refresh
    let result2 = client.refresh("test-refresh", &token1).await.unwrap();
    let token2 = result2.token.as_ref().unwrap().clone();
    assert_ne!(token1, token2);

    // Old token should fail
    let verify_old = client.verify("test-refresh", &token1).await;
    assert!(verify_old.is_err(), "old token should fail after refresh");

    // New token should work
    let verify_new = client.verify("test-refresh", &token2).await.unwrap();
    assert!(verify_new.credential_id.is_some());
}

#[tokio::test]
async fn client_hmac_issue_verify() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    // Create signing key
    client.rotate_force("test-hmac").await.unwrap();

    // Issue HMAC signature
    let payload = r#"{"data":"my-payload"}"#;
    let result = client
        .issue("test-hmac")
        .claims(serde_json::json!({"data": "my-payload"}))
        .execute()
        .await
        .unwrap();
    assert!(result.signature.is_some());

    let signature = result.signature.as_ref().unwrap();
    // HMAC verify returns kid (not credential_id), so just check it succeeds
    let _verify = client
        .verify_with_payload("test-hmac", signature, payload)
        .await
        .unwrap();
}

#[tokio::test]
async fn client_update_metadata() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result = client
        .issue("test-apikeys")
        .metadata(serde_json::json!({"env": "dev"}))
        .execute()
        .await
        .unwrap();
    let cred_id = result.credential_id.as_ref().unwrap();

    client
        .update("test-apikeys", cred_id, serde_json::json!({"env": "prod"}))
        .await
        .unwrap();
}

#[tokio::test]
async fn client_idempotency_key() {
    let server = common::TestServer::start().await;
    let mut client = KeyvaClient::connect(&server.addr.to_string())
        .await
        .unwrap();

    let result1 = client
        .issue("test-apikeys")
        .idempotency_key("idem-1")
        .execute()
        .await
        .unwrap();

    let result2 = client
        .issue("test-apikeys")
        .idempotency_key("idem-1")
        .execute()
        .await
        .unwrap();

    assert_eq!(
        result1.api_key, result2.api_key,
        "idempotent responses should match"
    );
}
