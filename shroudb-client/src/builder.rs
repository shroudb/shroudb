//! Fluent builder for the ISSUE command.

use crate::ShrouDBClient;
use crate::error::ClientError;
use crate::response::IssueResult;

/// Builder for constructing and executing an ISSUE command.
pub struct IssueBuilder<'a> {
    client: &'a mut ShrouDBClient,
    keyspace: String,
    claims: Option<serde_json::Value>,
    metadata: Option<serde_json::Value>,
    ttl: Option<u64>,
    idempotency_key: Option<String>,
}

impl<'a> IssueBuilder<'a> {
    pub(crate) fn new(client: &'a mut ShrouDBClient, keyspace: impl Into<String>) -> Self {
        Self {
            client,
            keyspace: keyspace.into(),
            claims: None,
            metadata: None,
            ttl: None,
            idempotency_key: None,
        }
    }

    /// Set JWT claims (for JWT keyspaces).
    pub fn claims(mut self, claims: serde_json::Value) -> Self {
        self.claims = Some(claims);
        self
    }

    /// Set credential metadata.
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Set time-to-live in seconds.
    pub fn ttl(mut self, secs: u64) -> Self {
        self.ttl = Some(secs);
        self
    }

    /// Set an idempotency key to prevent duplicate issuance.
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Execute the ISSUE command and return the result.
    pub async fn execute(self) -> Result<IssueResult, ClientError> {
        let mut args = vec!["ISSUE".to_string(), self.keyspace];
        if let Some(claims) = self.claims {
            args.push("CLAIMS".into());
            args.push(
                serde_json::to_string(&claims)
                    .map_err(|e| ClientError::Serialization(e.to_string()))?,
            );
        }
        if let Some(metadata) = self.metadata {
            args.push("META".into());
            args.push(
                serde_json::to_string(&metadata)
                    .map_err(|e| ClientError::Serialization(e.to_string()))?,
            );
        }
        if let Some(ttl) = self.ttl {
            args.push("TTL".into());
            args.push(ttl.to_string());
        }
        if let Some(key) = self.idempotency_key {
            args.push("IDEMPOTENCY_KEY".into());
            args.push(key);
        }
        let resp = self.client.connection.send_command(&args).await?;
        IssueResult::from_response(resp)
    }
}
