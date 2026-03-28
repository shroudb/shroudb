/// Protocol-agnostic command representation.
/// Produced by the RESP3 parser (or any future protocol adapter).
#[derive(Debug, Clone)]
pub enum Command {
    // ── Connection ───────────────────────────────────────────────────
    Auth {
        token: String,
    },
    Ping,

    // ── Data operations ──────────────────────────────────────────────
    Put {
        ns: String,
        key: Vec<u8>,
        value: Vec<u8>,
        metadata: Option<serde_json::Value>,
    },
    Get {
        ns: String,
        key: Vec<u8>,
        version: Option<u64>,
        meta: bool,
    },
    Delete {
        ns: String,
        key: Vec<u8>,
    },
    List {
        ns: String,
        prefix: Option<Vec<u8>>,
        cursor: Option<String>,
        limit: Option<usize>,
    },
    Versions {
        ns: String,
        key: Vec<u8>,
        limit: Option<usize>,
        from: Option<u64>,
    },

    // ── Namespace operations ─────────────────────────────────────────
    NamespaceCreate {
        name: String,
        schema: Option<serde_json::Value>,
        max_versions: Option<u64>,
        tombstone_retention_secs: Option<u64>,
    },
    NamespaceDrop {
        name: String,
        force: bool,
    },
    NamespaceList {
        cursor: Option<String>,
        limit: Option<usize>,
    },
    NamespaceInfo {
        name: String,
    },
    NamespaceAlter {
        name: String,
        schema: Option<serde_json::Value>,
        max_versions: Option<u64>,
        tombstone_retention_secs: Option<u64>,
    },
    NamespaceValidate {
        name: String,
    },

    // ── Batch ────────────────────────────────────────────────────────
    Pipeline {
        commands: Vec<Command>,
        request_id: Option<String>,
    },

    // ── Streaming ────────────────────────────────────────────────────
    Subscribe {
        ns: String,
        key: Option<Vec<u8>>,
        events: Vec<String>,
    },
    Unsubscribe,

    // ── Operational ──────────────────────────────────────────────────
    Health,
    ConfigGet {
        key: String,
    },
    ConfigSet {
        key: String,
        value: String,
    },
    CommandList,
}

// Re-export ACL types from shroudb-acl for use in acl_requirement().
pub use shroudb_acl::{AclRequirement, Scope};

impl Command {
    /// Determine the ACL requirement for this command.
    pub fn acl_requirement(&self) -> AclRequirement {
        match self {
            // Unscoped — allowed pre-auth or without grants
            Command::Auth { .. }
            | Command::Ping
            | Command::Health
            | Command::ConfigGet { .. }
            | Command::CommandList => AclRequirement::None,

            // Admin — global privilege
            Command::NamespaceCreate { .. }
            | Command::NamespaceDrop { .. }
            | Command::NamespaceAlter { .. }
            | Command::ConfigSet { .. } => AclRequirement::Admin,

            // Read — per-namespace
            Command::Get { ns, .. }
            | Command::List { ns, .. }
            | Command::Versions { ns, .. }
            | Command::NamespaceInfo { name: ns }
            | Command::NamespaceValidate { name: ns }
            | Command::Subscribe { ns, .. } => AclRequirement::Namespace {
                ns: ns.clone(),
                scope: Scope::Read,
                tenant_override: None,
            },

            // Write — per-namespace
            Command::Put { ns, .. } | Command::Delete { ns, .. } => AclRequirement::Namespace {
                ns: ns.clone(),
                scope: Scope::Write,
                tenant_override: None,
            },

            // NAMESPACE LIST returns only namespaces the token has grants on.
            // The filtering happens in the handler, not the ACL middleware.
            Command::NamespaceList { .. } => AclRequirement::None,

            // Unsubscribe has no namespace context (closes current subscription)
            Command::Unsubscribe => AclRequirement::None,

            // Pipeline: checked per-command during execution
            Command::Pipeline { .. } => AclRequirement::None,
        }
    }

    /// The command verb string (for metrics and audit).
    pub fn verb(&self) -> &'static str {
        match self {
            Command::Auth { .. } => "AUTH",
            Command::Ping => "PING",
            Command::Put { .. } => "PUT",
            Command::Get { .. } => "GET",
            Command::Delete { .. } => "DELETE",
            Command::List { .. } => "LIST",
            Command::Versions { .. } => "VERSIONS",
            Command::NamespaceCreate { .. } => "NAMESPACE CREATE",
            Command::NamespaceDrop { .. } => "NAMESPACE DROP",
            Command::NamespaceList { .. } => "NAMESPACE LIST",
            Command::NamespaceInfo { .. } => "NAMESPACE INFO",
            Command::NamespaceAlter { .. } => "NAMESPACE ALTER",
            Command::NamespaceValidate { .. } => "NAMESPACE VALIDATE",
            Command::Pipeline { .. } => "PIPELINE",
            Command::Subscribe { .. } => "SUBSCRIBE",
            Command::Unsubscribe => "UNSUBSCRIBE",
            Command::Health => "HEALTH",
            Command::ConfigGet { .. } => "CONFIG GET",
            Command::ConfigSet { .. } => "CONFIG SET",
            Command::CommandList => "COMMAND LIST",
        }
    }

    /// Whether this command is a read (does not modify state).
    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Command::Get { .. }
                | Command::List { .. }
                | Command::Versions { .. }
                | Command::NamespaceList { .. }
                | Command::NamespaceInfo { .. }
                | Command::NamespaceValidate { .. }
                | Command::Health
                | Command::ConfigGet { .. }
                | Command::CommandList
                | Command::Ping
        )
    }
}
