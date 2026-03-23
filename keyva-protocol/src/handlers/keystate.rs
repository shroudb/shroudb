use keyva_core::{Keyspace, KeyspaceType};
use keyva_storage::StorageEngine;
use keyva_storage::index::SigningKeyLike;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_keystate(
    engine: &StorageEngine,
    keyspace: &Keyspace,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;

    match keyspace.keyspace_type {
        KeyspaceType::Jwt => {
            let ring =
                engine
                    .index()
                    .jwt_rings
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    })?;

            let keys: Vec<ResponseValue> = ring
                .all_keys()
                .iter()
                .map(|k| {
                    ResponseValue::Map(
                        ResponseMap::ok()
                            .with(
                                "kid",
                                ResponseValue::String(k.key_id().as_str().to_string()),
                            )
                            .with("state", ResponseValue::String(format!("{:?}", k.state())))
                            .with("version", ResponseValue::Integer(k.version() as i64))
                            .with(
                                "algorithm",
                                ResponseValue::String(format!("{:?}", k.algorithm)),
                            )
                            .with("created_at", ResponseValue::Integer(k.created_at as i64)),
                    )
                })
                .collect();

            Ok(ResponseMap::ok().with("keys", ResponseValue::Array(keys)))
        }
        KeyspaceType::Hmac => {
            let ring =
                engine
                    .index()
                    .hmac_rings
                    .get(ks_name)
                    .ok_or_else(|| CommandError::NotFound {
                        entity: "keyring".into(),
                        id: ks_name.clone(),
                    })?;

            let keys: Vec<ResponseValue> = ring
                .all_keys()
                .iter()
                .map(|k| {
                    ResponseValue::Map(
                        ResponseMap::ok()
                            .with(
                                "kid",
                                ResponseValue::String(k.key_id().as_str().to_string()),
                            )
                            .with("state", ResponseValue::String(format!("{:?}", k.state())))
                            .with("version", ResponseValue::Integer(k.version() as i64))
                            .with(
                                "algorithm",
                                ResponseValue::String(format!("{:?}", k.algorithm)),
                            )
                            .with("created_at", ResponseValue::Integer(k.created_at as i64)),
                    )
                })
                .collect();

            Ok(ResponseMap::ok().with("keys", ResponseValue::Array(keys)))
        }
        _ => Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "jwt or hmac".into(),
        }),
    }
}
