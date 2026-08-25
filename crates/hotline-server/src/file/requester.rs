/// Which login a file operation is being performed on behalf of, so a
/// [`crate::FileProvider`] can decide what that login is allowed to see.
#[derive(Debug, Clone, Copy)]
pub struct FileRequester<'a> {
    /// The requesting session's account login.
    pub login: &'a str,
}
