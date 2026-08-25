/// What a session should do after [`FloodPolicy::check`](crate::FloodPolicy::check) records one more
/// transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodDecision {
    /// Under the limit; proceed as normal.
    Allow,
    /// Over the limit; drop this transaction and warn the sender instead.
    Throttle,
    /// Far enough over the limit to disconnect this session.
    Kick,
}
