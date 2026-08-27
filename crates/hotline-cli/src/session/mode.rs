/// Where the UI is: still showing the server's agreement, or fully
/// logged in and chatting.
pub enum Mode {
    Joining,
    Agreement(String),
    Chat,
}
