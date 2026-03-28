//! Protocol layer for ShrouDB.
//!
//! Command parsing, ACL enforcement, dispatch, handler execution,
//! and response serialization.

pub mod acl;
pub mod command;
pub mod dispatch;
pub mod error;
pub mod handlers;
pub mod resp3;
pub mod response;

pub use acl::AuthContext;
pub use command::Command;
pub use dispatch::CommandDispatcher;
pub use error::CommandError;
pub use resp3::{ProtocolError, Resp3Frame};
pub use response::{CommandResponse, ResponseMap, ResponseValue};

// Re-export key shroudb-acl types for convenience
pub use shroudb_acl::{AclError, Grant, Scope, StaticTokenValidator, Token, TokenValidator};
