use hotline_client::ClientUserInfo;
use hotline_proto::types::UserChangeOptions;

use super::TextBox;

/// The rows above the option rows: name, icon, and automatic response.
pub const TEXT_ROWS: usize = 3;

/// Each option and its label.
pub const OPTIONS: [(&str, UserChangeOptions); 3] = [
    ("refuse private messages", UserChangeOptions::REFUSE_PM),
    ("refuse private chat", UserChangeOptions::REFUSE_CHAT),
    (
        "send automatic response",
        UserChangeOptions::AUTOMATIC_RESPONSE,
    ),
];

/// The automatic response that the form gives for an empty one.
const DEFAULT_AUTOMATIC_RESPONSE: &str = "I'm not able to respond right now. Please try later!";

/// The label of the last row, which only `hlc` keeps.
pub const QUEUE_TRANSFERS: &str = "queue transfers";

/// This user's name, icon, and options being edited: three text rows, one
/// row for each option, and a row for [`QUEUE_TRANSFERS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsForm {
    pub name: TextBox,
    pub icon: TextBox,
    pub automatic_response: TextBox,
    pub options: UserChangeOptions,
    /// `true` if each transfer waits until each older one ends.
    pub queue_transfers: bool,
    /// The selected row.
    pub row: usize,
}

impl OptionsForm {
    /// A form that edits `info` and `queue_transfers`.
    pub fn edit(info: &ClientUserInfo, queue_transfers: bool) -> Self {
        Self {
            name: info.name.as_ref().into(),
            icon: info.icon.to_string().into(),
            automatic_response: info.automatic_response.as_ref().into(),
            options: info.options,
            queue_transfers,
            row: 0,
        }
    }

    /// The number of rows.
    pub const fn rows() -> usize {
        TEXT_ROWS.saturating_add(OPTIONS.len()).saturating_add(1)
    }

    /// The text of the selected row, `None` on an option row.
    pub const fn text_mut(&mut self) -> Option<&mut TextBox> {
        match self.row {
            0 => Some(&mut self.name),
            1 => Some(&mut self.icon),
            2 => Some(&mut self.automatic_response),
            _ => None,
        }
    }

    /// Turn the option on the selected row on or off.
    pub fn toggle(&mut self) {
        match OPTIONS.get(self.row.wrapping_sub(TEXT_ROWS)) {
            Some((_, flag)) => self.options.toggle(*flag),
            None if self.row == Self::rows().saturating_sub(1) => {
                self.queue_transfers = !self.queue_transfers;
            }
            None => {}
        }
    }

    /// The info to send, `None` if the icon is not a number. An empty
    /// automatic response that is on becomes [`DEFAULT_AUTOMATIC_RESPONSE`].
    pub fn info(&self) -> Option<ClientUserInfo> {
        Some(ClientUserInfo {
            name: self.name.as_str().into(),
            icon: self.icon.as_str().trim().parse().ok()?,
            options: self.options,
            automatic_response: match self.automatic_response.as_str() {
                "" if self.options.contains(UserChangeOptions::AUTOMATIC_RESPONSE) => {
                    DEFAULT_AUTOMATIC_RESPONSE.into()
                }
                text => text.into(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_needs_a_numeric_icon() {
        let mut form = OptionsForm::edit(&ClientUserInfo::default(), true);
        form.row = TEXT_ROWS + 2;
        form.toggle();
        assert_eq!(
            form.info().map(|info| info.options),
            Some(UserChangeOptions::AUTOMATIC_RESPONSE)
        );
        form.icon.insert('x');
        assert_eq!(form.info(), None);
    }

    #[test]
    fn info_gives_the_default_for_an_empty_automatic_response_that_is_on() {
        let mut form = OptionsForm::edit(&ClientUserInfo::default(), true);
        assert_eq!(form.info().unwrap().automatic_response, "".into());
        form.options = UserChangeOptions::AUTOMATIC_RESPONSE;
        assert_eq!(
            form.info().unwrap().automatic_response,
            DEFAULT_AUTOMATIC_RESPONSE.into()
        );
        form.automatic_response.insert('x');
        assert_eq!(form.info().unwrap().automatic_response, "x".into());
    }

    #[test]
    fn the_last_row_turns_the_queue_on_or_off() {
        let mut form = OptionsForm::edit(&ClientUserInfo::default(), true);
        form.row = OptionsForm::rows() - 1;
        form.toggle();
        assert!(!form.queue_transfers);
        assert_eq!(form.options, UserChangeOptions::empty());
    }
}
