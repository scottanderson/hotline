//! [`HlsPolicy`], the choices where `hls` differs from openline.

use std::borrow::Cow;

use hotline_server::{ServerPolicy, Silent, TakenLogin, TimeZone};

/// `hls` replies with an error in each case where openline sends none,
/// sends dates in [`TimeZone::Utc`], and refuses a rename onto a taken
/// login.
pub struct HlsPolicy;

impl ServerPolicy for HlsPolicy {
    fn error(&self, case: Silent) -> Option<Cow<'static, str>> {
        match case {
            Silent::MessageWithoutData => Some("missing message".into()),
            Silent::ChatRequest { reason } | Silent::DropBoxSource { reason } => Some(reason),
            Silent::CommentWrite { name, error } => {
                Some(format!("Cannot set info for \"{name}\":\r{error}").into())
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
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn error_names_the_file_of_a_failed_comment_write() {
        let case = Silent::CommentWrite {
            name: "a.txt".into(),
            error: io::Error::other("disk full"),
        };
        assert_eq!(
            HlsPolicy.error(case).as_deref(),
            Some("Cannot set info for \"a.txt\":\rdisk full")
        );
    }
}
