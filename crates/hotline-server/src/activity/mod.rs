//! Activity reports for an operator's logs, like openline's `myOpt_Log*`
//! options.
//!
//! The embedding application supplies an [`ActivityLog`] to
//! [`crate::Server::new`]. [`NoActivityLog`] records nothing.

mod entry;
mod log;
mod none;

pub use entry::{AccountAction, Activity, ActivityEntry};
pub use log::ActivityLog;
pub use none::NoActivityLog;
