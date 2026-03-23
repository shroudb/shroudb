use keyva_core::{Keyspace, KeyspaceType};
use keyva_storage::StorageEngine;

use crate::error::CommandError;
use crate::response::{ResponseMap, ResponseValue};

pub async fn handle_jwks(
    engine: &StorageEngine,
    keyspace: &Keyspace,
) -> Result<ResponseMap, CommandError> {
    let ks_name = &keyspace.name;

    if keyspace.keyspace_type != KeyspaceType::Jwt {
        return Err(CommandError::WrongType {
            keyspace: ks_name.clone(),
            actual: format!("{:?}", keyspace.keyspace_type),
            expected: "jwt".into(),
        });
    }

    let ring = engine
        .index()
        .jwt_rings
        .get(ks_name)
        .ok_or_else(|| CommandError::NotFound {
            entity: "keyring".into(),
            id: ks_name.clone(),
        })?;

    let mut jwks = Vec::new();
    for key in ring.verification_keys() {
        match keyva_crypto::public_key_to_jwk(
            key.algorithm,
            &key.public_key_der,
            key.key_id.as_str(),
        ) {
            Ok(jwk) => jwks.push(ResponseValue::Json(jwk)),
            Err(e) => {
                tracing::warn!(
                    kid = key.key_id.as_str(),
                    error = %e,
                    "failed to convert key to JWK"
                );
            }
        }
    }

    Ok(ResponseMap::ok().with("keys", ResponseValue::Array(jwks)))
}
