//! [`HlsPolicy`], the error replies that `hls` sends where openline sends
//! none.

use std::borrow::Cow;

use hotline_server::{ServerPolicy, Silent};

/// `hls` replies with an error in each case where openline sends none.
pub struct HlsPolicy;

impl ServerPolicy for HlsPolicy {
    fn error(&self, case: Silent) -> Option<Cow<'static, str>> {
        match case {
            Silent::MessageWithoutData => Some("missing message".into()),
            _ => None,
        }
    }
}
