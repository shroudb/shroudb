use std::time::{Instant, SystemTime, UNIX_EPOCH};

use metrics::histogram;
use shroudb_core::{ApiKeyState, KeyId, Keyspace, KeyspacePolicy, RefreshTokenState};
use shroudb_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_verify(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    token: &str,
    payload: Option<&str>,
    check_revoked: bool,
) -> Result<ResponseMap, CommandError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ks_name = &keyspace.name;

    match &keyspace.policy {
        KeyspacePolicy::Jwt {
            algorithm,
            required_claims,
            verify_cache_ttl_secs,
            leeway_secs,
            ..
        } => {
            // Decode header to get kid
            let header = shroudb_crypto::decode_jwt_header(token)?;
            let kid_str = header.kid.ok_or_else(|| CommandError::BadArg {
                message: "JWT missing kid header".into(),
            })?;
            let kid = KeyId::from_string(kid_str.clone());

            let ring =
                engine
                    .index()
                    .jwt_rings
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    })?;

            // Find key by kid (may be active or draining)
            let signing_key = ring
                .find_by_kid(&kid)
                .ok_or_else(|| CommandError::NotFound {
                    entity: "signing_key".into(),
                    id: kid_str.clone(),
                })?;

            // Verify signature and decode claims
            let claims = shroudb_crypto::verify_jwt(
                &signing_key.public_key_der,
                *algorithm,
                token,
                *leeway_secs,
            )?;

            // Validate exp claim (RFC 7519 §4.1.4)
            if let Some(exp) = claims.get("exp").and_then(|v| v.as_u64())
                && now > exp + leeway_secs
            {
                return Err(CommandError::Expired {
                    entity: "jwt".into(),
                    id: claims
                        .get("jti")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                });
            }

            // Validate nbf claim (RFC 7519 §4.1.5)
            if let Some(nbf) = claims.get("nbf").and_then(|v| v.as_u64())
                && now + leeway_secs < nbf
            {
                return Err(CommandError::ValidationError(
                    "token not yet valid (nbf)".into(),
                ));
            }

            // Check required claims
            if let Some(required) = required_claims {
                for (key, expected) in required {
                    let expected_json = expected.to_json();
                    match claims.get(key) {
                        None => {
                            return Err(CommandError::ValidationError(format!(
                                "missing required claim: {key}"
                            )));
                        }
                        Some(actual) => {
                            // RFC 7519 §4.1.3: aud can be an array of strings
                            let matches = if key == "aud" {
                                match actual {
                                    serde_json::Value::Array(arr) => arr.contains(&expected_json),
                                    other => *other == expected_json,
                                }
                            } else {
                                *actual == expected_json
                            };
                            if !matches {
                                return Err(CommandError::ValidationError(format!(
                                    "claim {key} mismatch: expected {expected_json}, got {actual}"
                                )));
                            }
                        }
                    }
                }
            }

            // Check revocation if requested
            if check_revoked {
                let rev_start = Instant::now();
                let revoked = if let Some(sub) = claims.get("jti").and_then(|v| v.as_str()) {
                    let cred_id = shroudb_core::CredentialId::from_string(sub.to_string());
                    engine
                        .index()
                        .revocations
                        .get(ks_name)
                        .is_some_and(|rev_set| rev_set.is_revoked(&cred_id))
                } else {
                    false
                };
                histogram!("shroudb_revocation_check_duration_seconds", "keyspace" => ks_name.to_string())
                    .record(rev_start.elapsed().as_secs_f64());
                if revoked {
                    return Err(CommandError::Denied {
                        reason: "token revoked".into(),
                    });
                }
            }

            let credential_id = claims
                .get("jti")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut resp = ResponseMap::ok()
                .with("claims", ResponseValue::Json(claims))
                .with("state", ResponseValue::String("active".into()));

            if let Some(cred_id) = credential_id {
                resp = resp.with("credential_id", ResponseValue::String(cred_id));
            }

            if let Some(cache_ttl) = verify_cache_ttl_secs {
                let cache_until = now + cache_ttl;
                resp = resp.with("cache_until", ResponseValue::Integer(cache_until as i64));
            }

            Ok(resp)
        }

        KeyspacePolicy::ApiKey { .. } => {
            let key_hash = shroudb_crypto::sha256(token.as_bytes());

            let idx =
                engine
                    .index()
                    .api_keys
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "index".into(),
                        id: ks_name.clone(),
                    })?;

            let entry = idx
                .lookup_by_hash(&key_hash)
                .ok_or_else(|| CommandError::NotFound {
                    entity: "api_key".into(),
                    id: "(hash lookup)".into(),
                })?;

            // Check state
            if entry.state != ApiKeyState::Active {
                return Err(CommandError::StateError {
                    from: entry.state.to_string(),
                    to: "verification requires Active".into(),
                });
            }

            // Check expiry
            if let Some(expires_at) = entry.expires_at
                && now > expires_at
            {
                return Err(CommandError::Expired {
                    entity: "api_key".into(),
                    id: entry.credential_id.as_str().to_string(),
                });
            }

            // Update last_verified_at (in-memory only)
            idx.update_last_verified(&key_hash, now);

            Ok(ResponseMap::ok()
                .with(
                    "credential_id",
                    ResponseValue::String(entry.credential_id.as_str().to_string()),
                )
                .with(
                    "meta",
                    ResponseValue::Json(shroudb_core::metadata_to_json(&entry.metadata)),
                )
                .with("state", ResponseValue::String("active".into())))
        }

        KeyspacePolicy::Hmac { algorithm, .. } => {
            let payload_bytes = payload
                .ok_or_else(|| CommandError::BadArg {
                    message: "HMAC VERIFY requires payload".into(),
                })?
                .as_bytes();

            let signature = hex::decode(token).map_err(|e| CommandError::BadArg {
                message: format!("invalid hex signature: {e}"),
            })?;

            let ring =
                engine
                    .index()
                    .hmac_rings
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    })?;

            // Try all verification keys (active + draining)
            for vkey in ring.verification_keys() {
                let key_material = match &vkey.key_material {
                    Some(km) => km,
                    None => continue,
                };
                let valid = shroudb_crypto::hmac_verify(
                    *algorithm,
                    key_material.as_bytes(),
                    payload_bytes,
                    &signature,
                )?;
                if valid {
                    return Ok(ResponseMap::ok()
                        .with(
                            "credential_id",
                            ResponseValue::String(vkey.key_id.as_str().to_string()),
                        )
                        .with("state", ResponseValue::String("active".into())));
                }
            }

            Err(CommandError::Denied {
                reason: "HMAC signature invalid".into(),
            })
        }

        KeyspacePolicy::RefreshToken { .. } => {
            let token_hash = shroudb_crypto::sha256(token.as_bytes());

            let idx = engine.index().refresh_tokens.get(ks_name).ok_or_else(|| {
                CommandError::NotFound {
                    entity: "index".into(),
                    id: ks_name.clone(),
                }
            })?;

            let entry = idx
                .lookup_by_hash(&token_hash)
                .ok_or_else(|| CommandError::NotFound {
                    entity: "refresh_token".into(),
                    id: "(hash lookup)".into(),
                })?;

            if entry.state != RefreshTokenState::Active {
                return Err(CommandError::StateError {
                    from: entry.state.to_string(),
                    to: "verification requires Active".into(),
                });
            }

            if now > entry.expires_at {
                return Err(CommandError::Expired {
                    entity: "refresh_token".into(),
                    id: entry.credential_id.as_str().to_string(),
                });
            }

            Ok(ResponseMap::ok()
                .with(
                    "credential_id",
                    ResponseValue::String(entry.credential_id.as_str().to_string()),
                )
                .with(
                    "family_id",
                    ResponseValue::String(entry.family_id.as_str().to_string()),
                )
                .with("state", ResponseValue::String("active".into())))
        }

        KeyspacePolicy::Password { .. } => Err(CommandError::WrongType {
            keyspace: keyspace.name.clone(),
            actual: "Password".into(),
            expected: "use PASSWORD VERIFY for password keyspaces".into(),
        }),
    }
}
