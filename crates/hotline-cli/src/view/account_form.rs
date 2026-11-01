use hotline_client::{Password, UserAccount};
use hotline_proto::Access;
use unicode_segmentation::UnicodeSegmentation;

use super::TextBox;

/// The rows above the privilege rows: name, login, and password.
pub const TEXT_ROWS: usize = 3;

/// Each privilege and its label, in bit order.
pub fn privileges() -> impl Iterator<Item = (String, Access)> {
    Access::all()
        .iter_names()
        .map(|(name, flag)| (name.to_lowercase().replace('_', " "), flag))
}

/// An account being edited: name, login, and password rows, then one row
/// for each privilege.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountForm {
    /// The login before the edit, `None` for a new account.
    pub old_login: Option<Box<str>>,
    pub name: TextBox,
    pub login: TextBox,
    /// The typed password, `None` while it is unchanged.
    pub password: Option<TextBox>,
    /// `true` if the account had a password before the edit.
    pub had_password: bool,
    pub access: Access,
    /// The selected row.
    pub row: usize,
}

impl AccountForm {
    /// A form for a new account.
    pub const fn blank() -> Self {
        Self {
            old_login: None,
            name: TextBox::new(),
            login: TextBox::new(),
            password: None,
            had_password: false,
            access: Access::empty(),
            row: 0,
        }
    }

    /// The privilege that saves the form: create user for a new account,
    /// or modify user for one that exists.
    pub const fn saved_by(&self) -> Access {
        if self.old_login.is_none() {
            Access::CREATE_USER
        } else {
            Access::MODIFY_USER
        }
    }

    /// A form that edits `account`.
    pub fn edit(account: &UserAccount) -> Self {
        Self {
            old_login: Some(account.login.clone()),
            name: account.name.as_ref().into(),
            login: account.login.as_ref().into(),
            had_password: account.password != Password::Empty,
            access: account.access,
            ..Self::blank()
        }
    }

    /// The number of rows.
    pub fn rows() -> usize {
        TEXT_ROWS + privileges().count()
    }

    /// The text of the selected row, `None` on a privilege row.
    pub fn text_mut(&mut self) -> Option<&mut TextBox> {
        match self.row {
            0 => Some(&mut self.name),
            1 => Some(&mut self.login),
            2 => Some(self.password.get_or_insert_with(TextBox::new)),
            _ => None,
        }
    }

    /// Turn the privilege on the selected row on or off.
    pub fn toggle(&mut self) {
        if let Some((_, flag)) = privileges().nth(self.row.wrapping_sub(TEXT_ROWS)) {
            self.access.toggle(flag);
        }
    }

    /// The password as the form shows it.
    pub fn password_text(&self) -> String {
        match (&self.password, self.had_password) {
            (Some(password), _) => "*".repeat(password.as_str().graphemes(true).count()),
            (None, true) => "(unchanged)".to_string(),
            (None, false) => "(none)".to_string(),
        }
    }

    /// The account to save.
    pub fn account(&self) -> UserAccount {
        let password = match (&self.password, self.had_password) {
            (Some(p), _) if !p.is_empty() => Password::New(p.as_str().into()),
            (None, true) => Password::Unchanged,
            _ => Password::Empty,
        };
        UserAccount {
            name: self.name.as_str().into(),
            login: self.login.as_str().into(),
            password,
            access: self.access,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use super::*;

    #[test]
    fn privileges_lists_each_named_bit_with_a_plain_label() {
        let first = privileges().next().unwrap();
        assert_eq!(first, ("delete file".to_string(), Access::DELETE_FILE));
        assert_eq!(privileges().count(), Access::all().iter().count());
    }

    #[test]
    fn an_untouched_password_stays_unchanged_and_an_emptied_one_clears() {
        let account = UserAccount {
            name: "Bo".into(),
            login: "bo".into(),
            password: Password::Unchanged,
            access: Access::READ_CHAT,
        };
        let mut form = AccountForm::edit(&account);
        assert_eq!(form.account(), account);

        form.row = 2;
        form.text_mut().unwrap().insert('x');
        assert_eq!(form.account().password, Password::New("x".into()));
        form.text_mut().unwrap().edit(KeyCode::Backspace.into());
        assert_eq!(form.account().password, Password::Empty);
    }

    #[test]
    fn password_text_shows_one_star_for_each_grapheme() {
        let mut form = AccountForm::blank();
        form.password = Some("e\u{301}x".into());
        assert_eq!(form.password_text(), "**");
    }

    #[test]
    fn toggle_flips_the_privilege_on_the_selected_row() {
        let mut form = AccountForm::blank();
        form.row = TEXT_ROWS + 1;
        form.toggle();
        assert_eq!(form.access, Access::UPLOAD_FILE);
        form.row = 0;
        form.toggle();
        assert_eq!(form.access, Access::UPLOAD_FILE);
    }
}
