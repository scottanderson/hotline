use super::ServerPolicy;

/// openline's behavior in every case.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenlinePolicy;

impl ServerPolicy for OpenlinePolicy {}
