use keyva_core::{KeyspacePolicy, KeyspaceType, SigningKeyState};
use keyva_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_health(
    engine: &StorageEngine,
    keyspace_name: Option<&str>,
) -> Result<ResponseMap, CommandError> {
    let state = engine.health();

    match keyspace_name {
        None => {
            // Global health
            Ok(ResponseMap::ok().with("state", ResponseValue::String(state.to_string())))
        }
        Some(ks_name) => {
            // Per-keyspace detailed health
            let ks =
                engine
                    .index()
                    .keyspaces
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyspace".into(),
                        id: ks_name.to_string(),
                    })?;

            let ks = ks.value().clone();

            let mut resp = ResponseMap::ok()
                .with("state", ResponseValue::String(state.to_string()))
                .with(
                    "keyspace_type",
                    ResponseValue::String(format!("{:?}", ks.keyspace_type)),
                )
                .with("disabled", ResponseValue::Boolean(ks.disabled));

            // Credential count from index
            let credential_count: i64 = match ks.keyspace_type {
                KeyspaceType::ApiKey => engine
                    .index()
                    .api_keys
                    .get(ks_name)
                    .map_or(0, |idx| idx.len() as i64),
                KeyspaceType::RefreshToken => engine
                    .index()
                    .refresh_tokens
                    .get(ks_name)
                    .map_or(0, |idx| idx.len() as i64),
                KeyspaceType::Jwt => engine
                    .index()
                    .jwt_rings
                    .get(ks_name)
                    .map_or(0, |ring| ring.len() as i64),
                KeyspaceType::Hmac => engine
                    .index()
                    .hmac_rings
                    .get(ks_name)
                    .map_or(0, |ring| ring.len() as i64),
                KeyspaceType::Password => engine
                    .index()
                    .passwords
                    .get(ks_name)
                    .map_or(0, |idx| idx.len() as i64),
            };
            resp = resp.with("credential_count", ResponseValue::Integer(credential_count));

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // For JWT keyspaces: active key age, next rotation, keys by state
            if let KeyspacePolicy::Jwt { rotation_days, .. } = &ks.policy
                && let Some(ring) = engine.index().jwt_rings.get(ks_name)
            {
                let all_keys = ring.all_keys();
                let mut by_state = ResponseMap::ok();
                for sk_state in &[
                    SigningKeyState::Staged,
                    SigningKeyState::Active,
                    SigningKeyState::Draining,
                    SigningKeyState::Retired,
                ] {
                    let count = all_keys.iter().filter(|k| k.state == *sk_state).count();
                    by_state = by_state.with(
                        format!("{sk_state:?}").to_lowercase(),
                        ResponseValue::Integer(count as i64),
                    );
                }
                resp = resp.with("keys_by_state", ResponseValue::Map(by_state));

                if let Some(active) = ring.active_key() {
                    let age_secs = now.saturating_sub(active.created_at);
                    resp = resp.with(
                        "active_key_age_secs",
                        ResponseValue::Integer(age_secs as i64),
                    );

                    let next_rotation = active.created_at + (*rotation_days as u64 * 86400);
                    resp = resp.with(
                        "next_rotation_at",
                        ResponseValue::Integer(next_rotation as i64),
                    );
                }
            }

            // For HMAC keyspaces: active key age, next rotation, keys by state
            if let KeyspacePolicy::Hmac { rotation_days, .. } = &ks.policy
                && let Some(ring) = engine.index().hmac_rings.get(ks_name)
            {
                let all_keys = ring.all_keys();
                let mut by_state = ResponseMap::ok();
                for sk_state in &[
                    SigningKeyState::Staged,
                    SigningKeyState::Active,
                    SigningKeyState::Draining,
                    SigningKeyState::Retired,
                ] {
                    let count = all_keys.iter().filter(|k| k.state == *sk_state).count();
                    by_state = by_state.with(
                        format!("{sk_state:?}").to_lowercase(),
                        ResponseValue::Integer(count as i64),
                    );
                }
                resp = resp.with("keys_by_state", ResponseValue::Map(by_state));

                if let Some(active) = ring.active_key() {
                    let age_secs = now.saturating_sub(active.created_at);
                    resp = resp.with(
                        "active_key_age_secs",
                        ResponseValue::Integer(age_secs as i64),
                    );

                    let next_rotation = active.created_at + (*rotation_days as u64 * 86400);
                    resp = resp.with(
                        "next_rotation_at",
                        ResponseValue::Integer(next_rotation as i64),
                    );
                }
            }

            // Revocation set size
            let revocation_count = engine
                .index()
                .revocations
                .get(ks_name)
                .map_or(0, |rev| rev.len() as i64);
            resp = resp.with("revocation_count", ResponseValue::Integer(revocation_count));

            Ok(resp)
        }
    }
}
