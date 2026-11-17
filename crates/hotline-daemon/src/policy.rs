//! [`HlsPolicy`], the choices where `hls` differs from openline.

use std::borrow::Cow;

use hotline_proto::consts::Access;
use hotline_server::{
    Account, ChatRooms, ServerPolicy, Silent, TakenLogin, TimeZone, TransferReport, UserEntry,
};

/// `hls` replies with an error in each case where openline sends none,
/// sends dates in [`TimeZone::Utc`], refuses a rename onto a taken login,
/// lets a user into a private chat room by invite or password, sends more
/// lines in the client info text, and keeps an account field that an edit
/// leaves out.
pub struct HlsPolicy;

impl ServerPolicy for HlsPolicy {
    fn error(&self, case: Silent) -> Option<Cow<'static, str>> {
        match case {
            Silent::MessageWithoutData => Some("missing message".into()),
            Silent::ChatRequest { reason } => Some(reason),
            Silent::NoArticleId => {
                Some("Cannot post article because the category has no article ID left.".into())
            }
            Silent::CommentWrite { name, error } => {
                Some(format!("Cannot set info for “{name}”:\r{error}").into())
            }
            _ => None,
        }
    }

    fn time_zone(&self) -> TimeZone {
        TimeZone::Utc
    }

    fn taken_login(&self) -> TakenLogin {
        TakenLogin::Refuse
    }

    fn chat_rooms(&self) -> ChatRooms {
        ChatRooms::Invited
    }

    fn client_info_text(&self, user: &UserEntry, transfers: &TransferReport) -> String {
        let online = user.connected_at.elapsed().as_secs();
        let version_line = user
            .client_version
            .map(|version| format!("Version:    {version}\r"))
            .unwrap_or_default();
        format!(
            "Nickname:   {name}\r\
             Name:       {account_name}\r\
             Account:    {login}\r\
             Icon:       {icon}\r\
             {version_line}\
             Address:    {addr}\r\
             Online:     {mins}m {secs}s\r\r{transfers}",
            name = user.name,
            account_name = or_default(&user.account_name, "(none)"),
            login = or_default(&user.login, "(guest)"),
            icon = user.icon,
            addr = user.remote_addr,
            mins = online / 60,
            secs = online % 60,
            transfers = transfers.text(),
        )
    }

    fn edit_account(
        &self,
        existing: Account,
        password: Option<Box<str>>,
        name: Option<String>,
        access: Option<Access>,
    ) -> Account {
        Account {
            password: password.unwrap_or(existing.password),
            name: name.map_or(existing.name, Into::into),
            access: access.unwrap_or(existing.access),
            access_extra: existing.access_extra,
        }
    }
}

/// `text`, or `default` when `text` is empty.
const fn or_default<'a>(text: &'a str, default: &'a str) -> &'a str {
    if text.is_empty() { default } else { text }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Instant;

    use hotline_server::AccessExtra;

    use super::*;

    fn bob(login: &str, account_name: &str, client_version: Option<u16>) -> UserEntry {
        UserEntry {
            user_id: 2,
            login: login.into(),
            account_name: account_name.into(),
            name: "Bob".into(),
            icon: 42,
            client_version,
            access: Access::empty(),
            access_extra: AccessExtra::default(),
            remote_addr: "192.0.2.1:5500".parse().unwrap(),
            connected_at: Instant::now(),
            last_active: Instant::now(),
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
        }
    }

    #[test]
    fn edit_account_keeps_a_field_that_the_request_leaves_out() {
        let existing = Account {
            password: "secret".into(),
            name: "Bob".into(),
            access: Access::SEND_CHAT,
            access_extra: AccessExtra::default(),
        };
        let kept = HlsPolicy.edit_account(existing.clone(), None, None, None);
        assert_eq!(*kept.name, *"Bob");
        assert_eq!(kept.access, Access::SEND_CHAT);
        let edited =
            HlsPolicy.edit_account(existing, None, Some("Robert".into()), Some(Access::empty()));
        assert_eq!(*edited.name, *"Robert");
        assert_eq!(edited.access, Access::empty());
        assert_eq!(*edited.password, *"secret");
    }

    #[test]
    fn client_info_text_has_more_lines() {
        let transfers = TransferReport::default();
        let text = HlsPolicy.client_info_text(&bob("bob", "Bobby", Some(185)), &transfers);
        assert_eq!(
            text,
            format!(
                "Nickname:   Bob\r\
                 Name:       Bobby\r\
                 Account:    bob\r\
                 Icon:       42\r\
                 Version:    185\r\
                 Address:    192.0.2.1:5500\r\
                 Online:     0m 0s\r\r{}",
                transfers.text()
            )
        );
    }

    #[test]
    fn client_info_text_names_an_empty_login_and_account_name() {
        let text = HlsPolicy.client_info_text(&bob("", "", None), &TransferReport::default());
        assert!(text.contains("Account:    (guest)"));
        assert!(text.contains("Name:       (none)"));
        assert!(!text.contains("Version:"));
    }

    #[test]
    fn error_names_the_file_of_a_failed_comment_write() {
        let case = Silent::CommentWrite {
            name: "a.txt".into(),
            error: io::Error::other("disk full"),
        };
        assert_eq!(
            HlsPolicy.error(case).as_deref(),
            Some("Cannot set info for “a.txt”:\rdisk full")
        );
    }
}
