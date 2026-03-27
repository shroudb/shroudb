use std::collections::HashMap;

use crate::command::Command;
use crate::error::CommandError;

/// A resolved auth policy for a connection/request.
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    pub name: String,
    pub keyspaces: Vec<String>,
    pub commands: Vec<String>,
}

impl AuthPolicy {
    /// A system-level policy that allows everything. Used by internal callers
    /// such as the rotation scheduler.
    pub fn system() -> Self {
        Self {
            name: "system".into(),
            keyspaces: vec!["*".into()],
            commands: vec!["*".into()],
        }
    }

    pub fn allows_keyspace(&self, ks: &str) -> bool {
        self.keyspaces.iter().any(|k| k == "*" || k == ks)
    }

    pub fn allows_command(&self, cmd: &str) -> bool {
        self.commands
            .iter()
            .any(|c| c == "*" || c.eq_ignore_ascii_case(cmd))
    }

    /// Check if this policy allows the given command.
    pub fn check(&self, command: &Command) -> Result<(), CommandError> {
        let verb = command_verb(command);
        if !self.allows_command(verb) {
            return Err(CommandError::Denied {
                reason: format!("command {verb} not allowed by policy '{}'", self.name),
            });
        }
        if let Some(ks) = command.keyspace()
            && !self.allows_keyspace(ks)
        {
            return Err(CommandError::Denied {
                reason: format!("keyspace '{ks}' not allowed by policy '{}'", self.name),
            });
        }
        Ok(())
    }
}

/// Registry of auth tokens to policies. Built from config at startup.
pub struct AuthRegistry {
    /// token string -> policy
    policies: HashMap<String, AuthPolicy>,
    /// Whether auth is required (method = "token")
    required: bool,
}

impl AuthRegistry {
    pub fn new(policies: HashMap<String, AuthPolicy>, required: bool) -> Self {
        Self { policies, required }
    }

    /// No auth configured — everything is allowed.
    pub fn permissive() -> Self {
        Self {
            policies: HashMap::new(),
            required: false,
        }
    }

    pub fn is_required(&self) -> bool {
        self.required
    }

    /// Look up a token and return the associated policy.
    pub fn authenticate(&self, token: &str) -> Result<&AuthPolicy, CommandError> {
        self.policies
            .get(token)
            .ok_or_else(|| CommandError::Denied {
                reason: "invalid token".into(),
            })
    }
}

pub fn command_verb(cmd: &Command) -> &'static str {
    match cmd {
        Command::Issue { .. } => "ISSUE",
        Command::Verify { .. } => "VERIFY",
        Command::Revoke { .. } => "REVOKE",
        Command::Refresh { .. } => "REFRESH",
        Command::Update { .. } => "UPDATE",
        Command::Inspect { .. } => "INSPECT",
        Command::Rotate { .. } => "ROTATE",
        Command::Jwks { .. } => "JWKS",
        Command::KeyState { .. } => "KEYSTATE",
        Command::Health { .. } => "HEALTH",
        Command::Keys { .. } => "KEYS",
        Command::Suspend { .. } => "SUSPEND",
        Command::Unsuspend { .. } => "UNSUSPEND",
        Command::Schema { .. } => "SCHEMA",
        Command::ConfigGet { .. } => "CONFIG",
        Command::ConfigSet { .. } => "CONFIG",
        Command::ConfigList => "CONFIG",
        Command::Subscribe { .. } => "SUBSCRIBE",
        Command::PasswordSet { .. } => "PASSWORD",
        Command::PasswordVerify { .. } => "PASSWORD",
        Command::PasswordChange { .. } => "PASSWORD",
        Command::PasswordReset { .. } => "PASSWORD",
        Command::PasswordImport { .. } => "PASSWORD",
        Command::KeyspaceCreate { .. } => "KEYSPACE_CREATE",
        Command::Auth { .. } => "AUTH",
        Command::Pipeline(_) => "PIPELINE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_policy(keyspaces: Vec<&str>, commands: Vec<&str>) -> AuthPolicy {
        AuthPolicy {
            name: "test".into(),
            keyspaces: keyspaces.into_iter().map(String::from).collect(),
            commands: commands.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn wildcard_allows_all() {
        let policy = make_policy(vec!["*"], vec!["*"]);
        assert!(policy.allows_keyspace("anything"));
        assert!(policy.allows_keyspace("other-ks"));
    }

    #[test]
    fn specific_keyspace_only() {
        let policy = make_policy(vec!["ks1"], vec!["*"]);
        assert!(policy.allows_keyspace("ks1"));
        assert!(!policy.allows_keyspace("ks2"));
    }

    #[test]
    fn wildcard_command_allows_all() {
        let policy = make_policy(vec!["*"], vec!["*"]);
        assert!(policy.allows_command("ISSUE"));
        assert!(policy.allows_command("VERIFY"));
        assert!(policy.allows_command("HEALTH"));
    }

    #[test]
    fn specific_commands_only() {
        let policy = make_policy(vec!["*"], vec!["VERIFY", "HEALTH"]);
        assert!(policy.allows_command("VERIFY"));
        assert!(policy.allows_command("HEALTH"));
        assert!(!policy.allows_command("ISSUE"));
    }

    #[test]
    fn authenticate_valid_token() {
        let policy = make_policy(vec!["*"], vec!["*"]);
        let mut policies = HashMap::new();
        policies.insert("secret123".to_string(), policy);
        let registry = AuthRegistry::new(policies, true);

        let result = registry.authenticate("secret123");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "test");
    }

    #[test]
    fn authenticate_invalid_token() {
        let registry = AuthRegistry::new(HashMap::new(), true);
        let result = registry.authenticate("bad-token");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CommandError::Denied { .. }));
    }

    #[test]
    fn permissive_not_required() {
        let registry = AuthRegistry::permissive();
        assert!(!registry.is_required());
    }
}
