//! [`HlsPolicy`], the choices where `hls` differs from openline.

use std::borrow::Cow;

use hotline_server::{ServerPolicy, Silent, TimeZone};

/// `hls` replies with an error in each case where openline sends none,
/// and sends dates in [`TimeZone::Utc`].
pub struct HlsPolicy;

impl ServerPolicy for HlsPolicy {
    fn error(&self, case: Silent) -> Option<Cow<'static, str>> {
        match case {
            Silent::MessageWithoutData => Some("missing message".into()),
            Silent::ChatRequest { reason } | Silent::DropBoxSource { reason } => Some(reason),
            Silent::CommentWrite { error } => Some(error.to_string().into()),
            _ => None,
        }
    }

    fn time_zone(&self) -> TimeZone {
        TimeZone::Utc
    }
}
