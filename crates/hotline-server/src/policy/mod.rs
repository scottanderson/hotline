//! [`ServerPolicy`] chooses the error replies where openline sends none.
//! [`OpenlinePolicy`] sends none, as openline does.

mod openline;
mod server_policy;

pub use openline::OpenlinePolicy;
pub use server_policy::{ServerPolicy, Silent};
