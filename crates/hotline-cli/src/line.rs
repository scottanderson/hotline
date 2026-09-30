/// One line in the scrollback: a chat message, a system notice (user
/// joined/left, a permission-denied notice, ...), or an error.
pub struct Line {
    pub text: Box<str>,
    pub kind: LineKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Chat,
    System,
    Error,
}
