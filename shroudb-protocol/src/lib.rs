//! Protocol layer for ShrouDB.
//!
//! Command parsing, dispatch, handler execution, and response serialization.

pub mod auth;
pub mod command;
pub mod dispatch;
pub mod error;
pub mod events;
pub mod handlers;
pub mod idempotency;
pub mod resp3;
pub mod response;
pub mod webhooks;

pub use auth::{AuthPolicy, AuthRegistry};
pub use command::{Command, ReplicaBehavior, RevokeTarget};
pub use dispatch::CommandDispatcher;
pub use error::CommandError;
pub use events::{EventBus, LifecycleEvent};
pub use idempotency::IdempotencyMap;
pub use resp3::{ProtocolError, Resp3Frame};
pub use response::{CommandResponse, ResponseMap, ResponseValue};
pub use webhooks::{WebhookConfig, WebhookDispatcher, WebhookEvent};
