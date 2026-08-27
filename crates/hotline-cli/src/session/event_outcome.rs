/// What the caller must do after applying one server event.
#[must_use]
pub enum EventOutcome {
    /// State updated; nothing further needed.
    Applied,
    /// Nothing to show for the agreement; the caller should call
    /// [`super::ClientSession::accept_agreement`] right away to complete
    /// login.
    NeedsAgreementAccept,
    /// The server ended the connection; the caller should stop its loop.
    Quit,
}
