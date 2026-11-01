use hotline_client::ClientUserInfo;
use hotline_proto::types::UserChangeOptions;

use super::TextBox;

/// The rows above the option rows: name, icon, and automatic response.
pub const TEXT_ROWS: usize = 3;

/// Each option and its label.
pub const OPTIONS: [(&str, UserChangeOptions); 3] = [
    ("refuse private messages", UserChangeOptions::REFUSE_PM),
    ("refuse private chat", UserChangeOptions::REFUSE_CHAT),
    ("send automatic response", UserChangeOptions::AUTO_RESPONSE),
];

/// This user's name, icon, and options being edited: three text rows, then
/// one row for each option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsForm {
    pub name: TextBox,
    pub icon: TextBox,
    pub automatic_response: TextBox,
    pub options: UserChangeOptions,
    /// The selected row.
    pub row: usize,
}

impl OptionsForm {
    /// A form that edits `info`.
    pub fn edit(info: &ClientUserInfo) -> Self {
        Self {
            name: info.name.as_ref().into(),
            icon: info.icon.to_string().into(),
            automatic_response: info.automatic_response.as_ref().into(),
            options: info.options,
            row: 0,
        }
    }

    /// The number of rows.
    pub const fn rows() -> usize {
        TEXT_ROWS + OPTIONS.len()
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
        if let Some((_, flag)) = OPTIONS.get(self.row.wrapping_sub(TEXT_ROWS)) {
            self.options.toggle(*flag);
        }
    }

    /// The info to send, `None` if the icon is not a number.
    pub fn info(&self) -> Option<ClientUserInfo> {
        Some(ClientUserInfo {
            name: self.name.as_str().into(),
            icon: self.icon.as_str().trim().parse().ok()?,
            options: self.options,
            automatic_response: self.automatic_response.as_str().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_needs_a_numeric_icon() {
        let mut form = OptionsForm::edit(&ClientUserInfo::default());
        form.row = TEXT_ROWS + 2;
        form.toggle();
        assert_eq!(
            form.info().map(|info| info.options),
            Some(UserChangeOptions::AUTO_RESPONSE)
        );
        form.icon.insert('x');
        assert_eq!(form.info(), None);
    }
}
