use unicode_segmentation::UnicodeSegmentation;

use super::TextBox;
use crate::connect::DEFAULT_PORT;

/// A server to connect to, and the login on it: three text rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectForm {
    /// `host` or `host:port`.
    pub address: TextBox,
    pub login: TextBox,
    pub password: TextBox,
    /// The selected row.
    pub row: usize,
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
    fn password_text_shows_one_star_for_each_grapheme() {
        let form = ConnectForm::new("", "", "e\u{301}x");
        assert_eq!(form.password_text(), "**");
    }
}
