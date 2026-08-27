use async_trait::async_trait;

use super::{ChatCommandActor, ConsoleReply};
use crate::state::SharedState;

/// Maps a console command token to a handler.
#[async_trait]
pub trait ChatCommandDispatcher: Send + Sync {
    /// Runs `command` (lowercased, without the prefix) with the rest of the
    /// line as `args`. `None` if this command isn't recognized.
    async fn run(
        &self,
        command: &str,
        args: &str,
        actor: &ChatCommandActor,
        state: &SharedState,
    ) -> Option<ConsoleReply>;
}
