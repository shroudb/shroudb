//! Fluent builders for commands with many optional parameters.

use crate::ShrouDBClient;
use crate::error::ClientError;
use crate::response::{OkResult, VerifyResult};

// ---------------------------------------------------------------------------
// VerifyBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing and executing a VERIFY command.
pub struct VerifyBuilder<'a> {
    client: &'a mut ShrouDBClient,
    keyspace: String,
    token: String,
    payload: Option<String>,
    check_revoked: bool,
}

impl<'a> VerifyBuilder<'a> {
    pub(crate) fn new(
        client: &'a mut ShrouDBClient,
        keyspace: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            client,
            keyspace: keyspace.into(),
            token: token.into(),
            payload: None,
            check_revoked: false,
        }
    }

    /// Set the payload for HMAC verification.
    pub fn payload(mut self, payload: impl Into<String>) -> Self {
        self.payload = Some(payload.into());
        self
    }

    /// Enable revocation checking.
    pub fn check_revoked(mut self) -> Self {
        self.check_revoked = true;
        self
    }

    /// Execute the VERIFY command and return the result.
    pub async fn execute(self) -> Result<VerifyResult, ClientError> {
        let mut args = vec!["VERIFY".to_string(), self.keyspace, self.token];
        if let Some(payload) = self.payload {
            args.push("PAYLOAD".into());
            args.push(payload);
        }
        if self.check_revoked {
            args.push("CHECKREV".into());
        }
        let resp = self.client.connection.send_command(&args).await?;
        VerifyResult::from_response(resp)
    }
}

// ---------------------------------------------------------------------------
// RevokeBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing and executing a REVOKE command.
pub struct RevokeBuilder<'a> {
    client: &'a mut ShrouDBClient,
    keyspace: String,
    target: RevokeTarget,
    ttl_secs: Option<u64>,
}

enum RevokeTarget {
    Single(String),
    Family(String),
    Bulk(Vec<String>),
}

impl<'a> RevokeBuilder<'a> {
    pub(crate) fn new_single(
        client: &'a mut ShrouDBClient,
        keyspace: impl Into<String>,
        credential_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            keyspace: keyspace.into(),
            target: RevokeTarget::Single(credential_id.into()),
            ttl_secs: None,
        }
    }

    pub(crate) fn new_family(
        client: &'a mut ShrouDBClient,
        keyspace: impl Into<String>,
        family_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            keyspace: keyspace.into(),
            target: RevokeTarget::Family(family_id.into()),
            ttl_secs: None,
        }
    }

    pub(crate) fn new_bulk(
        client: &'a mut ShrouDBClient,
        keyspace: impl Into<String>,
        ids: Vec<String>,
    ) -> Self {
        Self {
            client,
            keyspace: keyspace.into(),
            target: RevokeTarget::Bulk(ids),
            ttl_secs: None,
        }
    }

    /// Set how long the revocation record is retained (in seconds).
    pub fn ttl(mut self, secs: u64) -> Self {
        self.ttl_secs = Some(secs);
        self
    }

    /// Execute the REVOKE command.
    pub async fn execute(self) -> Result<(), ClientError> {
        let mut args = vec!["REVOKE".to_string(), self.keyspace];
        match self.target {
            RevokeTarget::Single(id) => args.push(id),
            RevokeTarget::Family(fid) => {
                args.push("FAMILY".into());
                args.push(fid);
            }
            RevokeTarget::Bulk(ids) => {
                args.push("BULK".into());
                args.extend(ids);
            }
        }
        if let Some(ttl) = self.ttl_secs {
            args.push("TTL".into());
            args.push(ttl.to_string());
        }
        let resp = self.client.connection.send_command(&args).await?;
        crate::check_ok_status(resp)
    }
}

// ---------------------------------------------------------------------------
// KeysBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing and executing a KEYS command with pagination and filtering.
pub struct KeysBuilder<'a> {
    client: &'a mut ShrouDBClient,
    keyspace: String,
    cursor: Option<String>,
    pattern: Option<String>,
    state_filter: Option<String>,
    count: Option<usize>,
}

impl<'a> KeysBuilder<'a> {
    pub(crate) fn new(client: &'a mut ShrouDBClient, keyspace: impl Into<String>) -> Self {
        Self {
            client,
            keyspace: keyspace.into(),
            cursor: None,
            pattern: None,
            state_filter: None,
            count: None,
        }
    }

    /// Resume from a previous scan position.
    pub fn cursor(mut self, cursor: impl Into<String>) -> Self {
        self.cursor = Some(cursor.into());
        self
    }

    /// Filter by credential ID pattern (glob-style).
    pub fn pattern(mut self, pattern: impl Into<String>) -> Self {
        self.pattern = Some(pattern.into());
        self
    }

    /// Filter by credential state: `"active"`, `"suspended"`, `"revoked"`.
    pub fn state(mut self, state: impl Into<String>) -> Self {
        self.state_filter = Some(state.into());
        self
    }

    /// Maximum number of results to return (default: 100).
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// Execute the KEYS command and return the result.
    pub async fn execute(self) -> Result<OkResult, ClientError> {
        let mut args = vec!["KEYS".to_string(), self.keyspace];
        if let Some(cursor) = self.cursor {
            args.push("CURSOR".into());
            args.push(cursor);
        }
        if let Some(pattern) = self.pattern {
            args.push("MATCH".into());
            args.push(pattern);
        }
        if let Some(state) = self.state_filter {
            args.push("STATE".into());
            args.push(state);
        }
        if let Some(count) = self.count {
            args.push("COUNT".into());
            args.push(count.to_string());
        }
        let resp = self.client.connection.send_command(&args).await?;
        OkResult::from_response(resp)
    }
}
