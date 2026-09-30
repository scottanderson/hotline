/// What the caller must do after one key press or mouse click.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Send public chat, sent unchanged even when it starts with `/`.
    SendChat(String),
    SendInstantMsg {
        user_id: u16,
        text: String,
    },
    GetClientInfoText(u16),
    GetMsgs,
    OldPostNews(String),
}
