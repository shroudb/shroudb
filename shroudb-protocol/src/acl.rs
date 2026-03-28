//! ACL integration — re-exports from `shroudb-acl` for protocol-level use.

pub use shroudb_acl::{
    AclError, AclRequirement, AuthContext, Grant, Scope, StaticTokenValidator, Token, TokenGrant,
    TokenValidator,
};
