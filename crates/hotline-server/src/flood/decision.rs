/// What happens after [`FloodPolicy`](crate::FloodPolicy) records an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodDecision {
    /// Under every limit; proceed as normal.
    Allow,
    /// Over a limit; ban the address for a while and disconnect it.
    Ban,
}

impl FloodDecision {
    /// [`Self::Ban`] if `over` a limit, else [`Self::Allow`].
    pub(crate) const fn ban_if(over: bool) -> Self {
        if over { Self::Ban } else { Self::Allow }
    }
}
