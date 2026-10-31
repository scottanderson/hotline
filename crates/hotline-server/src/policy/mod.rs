//! [`ServerPolicy`] holds the choices where a server differs from
//! openline. [`OpenlinePolicy`] makes each choice as openline does.

mod openline;
mod server_policy;
mod time_zone;

pub use openline::OpenlinePolicy;
pub use server_policy::{ServerPolicy, Silent};
pub(crate) use time_zone::to_utc;
pub use time_zone::TimeZone;
