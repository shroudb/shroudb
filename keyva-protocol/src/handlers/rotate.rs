use std::time::{SystemTime, UNIX_EPOCH};

use keyva_core::{KeyId, Keyspace, KeyspacePolicy, SigningKeyState};
use keyva_storage::wal::{SigningKeyAlgorithm, SigningKeyCreatedPayload};
use keyva_storage::{OpType, StorageEngine, WalPayload};

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_rotate(
    engine: &StorageEngine,
    keyspace: &Keyspace,
    force: bool,
    dryrun: bool,
) -> Result<ResponseMap, CommandError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ks_name = &keyspace.name;

    match &keyspace.policy {
        KeyspacePolicy::Jwt {
            algorithm,
            rotation_days,
            pre_stage_days,
            ..
        } => {
            let (active, staged, all_keys_len) = {
                let ring = engine.index().jwt_rings.get(ks_name).ok_or_else(|| {
                    CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    }
                })?;
                (ring.active_key(), ring.staged_key(), ring.all_keys().len())
            };

            // Schedule check: skip rotation if not yet due (unless forced)
            if !force && let Some(ref active) = active {
                let rotation_due_at = active.created_at + (*rotation_days as u64 * 86400);
                if now < rotation_due_at {
                    // Pre-staging: if within pre_stage_days of rotation and no staged key, generate one
                    let pre_stage_at = rotation_due_at - (*pre_stage_days as u64 * 86400);
                    if now >= pre_stage_at && staged.is_none() {
                        let kp = keyva_crypto::generate_signing_key(*algorithm)?;
                        let new_kid = KeyId::new();
                        let version = all_keys_len as u32 + 1;

                        let encrypted_private_key =
                            engine.encrypt_private_key(ks_name, kp.private_key_pkcs8.as_bytes())?;

                        let payload = SigningKeyCreatedPayload {
                            key_id: new_kid.clone(),
                            algorithm: SigningKeyAlgorithm::Jwt(*algorithm),
                            state: SigningKeyState::Staged,
                            public_key_der: kp.public_key_der.clone(),
                            encrypted_private_key,
                            created_at: now,
                            version,
                        };

                        engine
                            .apply(
                                ks_name,
                                OpType::SigningKeyCreated,
                                WalPayload::SigningKeyCreated(payload),
                            )
                            .await?;

                        // Patch private key into in-memory ring
                        if let Some(ring) = engine.index().jwt_rings.get(ks_name)
                            && let Some(mut key) = ring.find_by_kid(&new_kid)
                        {
                            key.private_key = Some(kp.private_key_pkcs8);
                            ring.replace(key);
                        }

                        return Ok(ResponseMap::ok()
                            .with("message", ResponseValue::String("pre-staged key".into()))
                            .with(
                                "new_kid",
                                ResponseValue::String(new_kid.as_str().to_string()),
                            )
                            .with(
                                "next_rotation_at",
                                ResponseValue::Integer(rotation_due_at as i64),
                            ));
                    }

                    return Ok(ResponseMap::ok()
                        .with("message", ResponseValue::String("rotation not due".into()))
                        .with(
                            "next_rotation_at",
                            ResponseValue::Integer(rotation_due_at as i64),
                        ));
                }
            }

            if dryrun {
                let mut plan = Vec::new();
                if let Some(ref a) = active {
                    plan.push(format!("demote {} Active->Draining", a.key_id.as_str()));
                }
                if let Some(ref s) = staged {
                    plan.push(format!("promote {} Staged->Active", s.key_id.as_str()));
                }
                plan.push("generate new Staged key".into());
                return Ok(ResponseMap::ok().with(
                    "plan",
                    ResponseValue::Array(plan.into_iter().map(ResponseValue::String).collect()),
                ));
            }

            // Promote staged -> active if present
            if let Some(s) = &staged {
                engine
                    .apply(
                        ks_name,
                        OpType::SigningKeyActivated,
                        WalPayload::SigningKeyStateChanged {
                            key_id: s.key_id.clone(),
                            new_state: SigningKeyState::Active,
                        },
                    )
                    .await?;
            }

            // Demote active -> draining if present
            if let Some(a) = &active {
                engine
                    .apply(
                        ks_name,
                        OpType::SigningKeyDraining,
                        WalPayload::SigningKeyStateChanged {
                            key_id: a.key_id.clone(),
                            new_state: SigningKeyState::Draining,
                        },
                    )
                    .await?;
            }

            // Generate new key: Active if no staged key was promoted to Active, Staged otherwise
            let kp = keyva_crypto::generate_signing_key(*algorithm)?;
            let new_kid = KeyId::new();
            let version = all_keys_len as u32 + 1;
            let new_state = if staged.is_none() {
                SigningKeyState::Active
            } else {
                SigningKeyState::Staged
            };

            let encrypted_private_key =
                engine.encrypt_private_key(ks_name, kp.private_key_pkcs8.as_bytes())?;

            let payload = SigningKeyCreatedPayload {
                key_id: new_kid.clone(),
                algorithm: SigningKeyAlgorithm::Jwt(*algorithm),
                state: new_state,
                public_key_der: kp.public_key_der.clone(),
                encrypted_private_key,
                created_at: now,
                version,
            };

            engine
                .apply(
                    ks_name,
                    OpType::SigningKeyCreated,
                    WalPayload::SigningKeyCreated(payload),
                )
                .await?;

            // Patch the in-memory key with private key material (apply_payload_to_index
            // sets private_key to None since it's designed for recovery)
            if let Some(ring) = engine.index().jwt_rings.get(ks_name)
                && let Some(mut key) = ring.find_by_kid(&new_kid)
            {
                key.private_key = Some(kp.private_key_pkcs8);
                ring.replace(key);
            }

            Ok(ResponseMap::ok().with(
                "new_kid",
                ResponseValue::String(new_kid.as_str().to_string()),
            ))
        }

        KeyspacePolicy::Hmac {
            algorithm,
            rotation_days,
            ..
        } => {
            let (active, staged, all_keys_len) = {
                let ring = engine.index().hmac_rings.get(ks_name).ok_or_else(|| {
                    CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    }
                })?;
                (ring.active_key(), ring.staged_key(), ring.all_keys().len())
            };

            // Schedule check: skip rotation if not yet due (unless forced)
            if !force && let Some(ref active) = active {
                let rotation_due_at = active.created_at + (*rotation_days as u64 * 86400);
                if now < rotation_due_at {
                    return Ok(ResponseMap::ok()
                        .with("message", ResponseValue::String("rotation not due".into()))
                        .with(
                            "next_rotation_at",
                            ResponseValue::Integer(rotation_due_at as i64),
                        ));
                }
            }

            if dryrun {
                let mut plan = Vec::new();
                if let Some(ref a) = active {
                    plan.push(format!("demote {} Active->Draining", a.key_id.as_str()));
                }
                if let Some(ref s) = staged {
                    plan.push(format!("promote {} Staged->Active", s.key_id.as_str()));
                }
                plan.push("generate new Staged HMAC key".into());
                return Ok(ResponseMap::ok().with(
                    "plan",
                    ResponseValue::Array(plan.into_iter().map(ResponseValue::String).collect()),
                ));
            }

            // Promote staged -> active
            if let Some(s) = &staged {
                engine
                    .apply(
                        ks_name,
                        OpType::SigningKeyActivated,
                        WalPayload::SigningKeyStateChanged {
                            key_id: s.key_id.clone(),
                            new_state: SigningKeyState::Active,
                        },
                    )
                    .await?;
            }

            // Demote active -> draining
            if let Some(a) = &active {
                engine
                    .apply(
                        ks_name,
                        OpType::SigningKeyDraining,
                        WalPayload::SigningKeyStateChanged {
                            key_id: a.key_id.clone(),
                            new_state: SigningKeyState::Draining,
                        },
                    )
                    .await?;
            }

            // Generate new HMAC key: Active if no staged key was promoted, Staged otherwise
            let (_, raw_bytes) = keyva_crypto::generate_api_key(None)?;
            let new_kid = KeyId::new();
            let version = all_keys_len as u32 + 1;
            let new_state = if staged.is_none() {
                SigningKeyState::Active
            } else {
                SigningKeyState::Staged
            };

            let encrypted_private_key = engine.encrypt_private_key(ks_name, &raw_bytes)?;

            let payload = SigningKeyCreatedPayload {
                key_id: new_kid.clone(),
                algorithm: SigningKeyAlgorithm::Hmac(*algorithm),
                state: new_state,
                public_key_der: vec![], // HMAC has no public key
                encrypted_private_key,
                created_at: now,
                version,
            };

            engine
                .apply(
                    ks_name,
                    OpType::SigningKeyCreated,
                    WalPayload::SigningKeyCreated(payload),
                )
                .await?;

            // Patch HMAC key material into in-memory ring (apply_payload_to_index
            // sets key_material to None since it's designed for recovery)
            if let Some(ring) = engine.index().hmac_rings.get(ks_name)
                && let Some(mut key) = ring.find_by_kid(&new_kid)
            {
                key.key_material = Some(keyva_crypto::SecretBytes::new(raw_bytes.to_vec()));
                ring.replace(key);
            }

            Ok(ResponseMap::ok().with(
                "new_kid",
                ResponseValue::String(new_kid.as_str().to_string()),
            ))
        }

        _ => Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "jwt or hmac".into(),
        }),
    }
}
