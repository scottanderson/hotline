//! Console commands: a chat message beginning with
//! [`crate::ServerConfig::console_prefix`] dispatches here instead of
//! relaying as ordinary chat text.
//!
//! A command target is a user id or a nickname. mhxd's `/color` and `/mon`
//! are not ported.

mod actor;
mod default;
mod dispatcher;
pub(crate) mod fields;
mod reply;
mod strtoul;

pub use actor::ChatCommandActor;
pub use default::DefaultChatCommands;
pub use dispatcher::ChatCommandDispatcher;
pub use reply::ConsoleReply;
pub(crate) use strtoul::strtoul;
