/// Which of the two node kinds a news path resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsItemKind {
    Bundle,
    Category,
}
