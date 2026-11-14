use unicode_segmentation::UnicodeSegmentation;

use super::TextBox;
use crate::config::Bookmark;
use crate::connect::{DEFAULT_PORT, short_address};

/// A server to connect to, and the login on it: three text rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectForm {
    /// `host` or `host:port`.
    pub address: TextBox,
    pub login: TextBox,
    pub password: TextBox,
    /// The selected row.
    pub row: usize,
    /// The name that [`ConnectForm::bookmark`] gives. Empty for the
    /// default name.
    pub name: Box<str>,
}

impl ConnectForm {
    /// The number of rows.
    pub const ROWS: usize = 3;

    /// A form filled with `address`, `login`, and `password`.
    pub fn new(address: &str, login: &str, password: &str) -> Self {
        Self {
            address: address.into(),
            login: login.into(),
            password: password.into(),
            row: 0,
            name: "".into(),
        }
    }

    /// Select the next row, or the first row after the last.
    pub const fn next_row(&mut self) {
        self.row = self.row.saturating_add(1) % Self::ROWS;
    }

    /// Select the previous row, or the last row before the first.
    pub fn previous_row(&mut self) {
        self.row = self.row.checked_sub(1).unwrap_or(Self::ROWS - 1);
    }

    /// The text of the selected row.
    pub const fn text_mut(&mut self) -> &mut TextBox {
        match self.row {
            0 => &mut self.address,
            1 => &mut self.login,
            _ => &mut self.password,
        }
    }

    /// The password as the form shows it: a star for each character.
    pub fn password_text(&self) -> String {
        "*".repeat(self.password.as_str().graphemes(true).count())
    }

    /// The host and the port of the address, with [`DEFAULT_PORT`] when it
    /// names no port. `None` when the host is blank or the port is not a
    /// number. An IPv6 host with a port is in brackets, as in `[::1]:5500`.
    pub fn server(&self) -> Option<(String, u16)> {
        let text = self.address.as_str().trim();
        let (host, port) = match text.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') || host.ends_with(']') => {
                (host, port.parse().ok()?)
            }
            _ => (text, DEFAULT_PORT),
        };
        (!host.is_empty()).then(|| host.to_string()).zip(Some(port))
    }

    /// The address of the bookmark: `host` or `host:port`, without the
    /// port [`DEFAULT_PORT`]. `None` when [`ConnectForm::server`] refuses
    /// the address.
    fn bookmark_address(&self) -> Option<String> {
        self.server().map(|(host, port)| short_address(&host, port))
    }

    /// The name of the bookmark when [`ConnectForm::name`] is empty:
    /// `[login@]host[:port]`, without an empty login.
    pub fn default_name(&self) -> Option<Box<str>> {
        let address = self.bookmark_address()?;
        Some(Bookmark::default_name(self.login.as_str(), &address).into())
    }

    /// The form as a bookmark. An empty name, or the default name, gives
    /// the bookmark no name of its own. `None` when [`ConnectForm::server`]
    /// refuses the address.
    pub fn bookmark(&self) -> Option<Bookmark> {
        let default = self.default_name()?;
        let named = !self.name.is_empty() && self.name != default;
        Some(Bookmark {
            name: named.then(|| self.name.clone()),
            address: self.bookmark_address()?.into(),
            login: self.login.as_str().into(),
            password: self.password.as_str().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(address: &str) -> Option<(String, u16)> {
        ConnectForm::new(address, "", "").server()
    }

    #[test]
    fn server_takes_the_port_after_the_last_colon() {
        assert_eq!(
            server("example.com:6000"),
            Some(("example.com".into(), 6000))
        );
        assert_eq!(server(" example.com "), Some(("example.com".into(), 5500)));
        assert_eq!(server("[::1]:6000"), Some(("[::1]".into(), 6000)));
        assert_eq!(server("[::1]"), Some(("[::1]".into(), 5500)));
    }

    #[test]
    fn server_refuses_a_blank_host_or_a_bad_port() {
        assert_eq!(server(""), None);
        assert_eq!(server(":6000"), None);
        assert_eq!(server("example.com:x"), None);
        assert_eq!(server("example.com:70000"), None);
    }

    #[test]
    fn bookmark_takes_its_name_from_the_login_and_the_address() {
        let form = ConnectForm::new(" example.com:6000 ", "alice", "secret");
        assert_eq!(
            form.bookmark(),
            Some(Bookmark {
                name: None,
                address: "example.com:6000".into(),
                login: "alice".into(),
                password: "secret".into(),
            })
        );
    }

    #[test]
    fn bookmark_takes_the_name_of_the_form() {
        let mut form = ConnectForm::new("example.com", "alice", "");
        form.name = "Home".into();
        assert_eq!(form.bookmark().unwrap().name.as_deref(), Some("Home"));
        form.name = "alice@example.com".into();
        assert_eq!(form.bookmark().unwrap().name, None);
    }

    #[test]
    fn bookmark_leaves_out_an_empty_login_and_the_default_port() {
        let form = ConnectForm::new("example.com:5500", "", "");
        assert_eq!(
            form.bookmark(),
            Some(Bookmark {
                name: None,
                address: "example.com".into(),
                login: "".into(),
                password: "".into(),
            })
        );
    }

    #[test]
    fn bookmark_refuses_a_bad_address() {
        assert_eq!(ConnectForm::new("example.com:x", "", "").bookmark(), None);
    }

    #[test]
    fn password_text_shows_one_star_for_each_grapheme() {
        let form = ConnectForm::new("", "", "e\u{301}x");
        assert_eq!(form.password_text(), "**");
    }
}
