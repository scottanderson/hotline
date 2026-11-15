use hotline_proto::htxf::FlatFileInfo;

use super::{ServerPolicy, TextLimits};
use crate::flood::MAX_CHAT_LINE;

/// openline's behavior in every case.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenlinePolicy;

impl ServerPolicy for OpenlinePolicy {
    fn text_limits(&self) -> TextLimits {
        TextLimits {
            account_name: Some(64),
            login: Some(32),
            password: Some(32),
            kept_password: Some(31),
            old_login: Some(31),
            user_name: Some(31),
            chat_line: Some(MAX_CHAT_LINE),
            chat_subject: Some(255),
            automatic_response: Some(127),
            ban_note: Some(63),
            message: Some(4096),
            file_comment: Some(FlatFileInfo::MAX_COMMENT_LEN),
        }
    }
}
