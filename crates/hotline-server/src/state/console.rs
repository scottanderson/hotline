use hotline_proto::consts::Access;

use crate::account::AccessExtra;

/// A console command's live change to one connected session. The account
/// is not changed.
#[derive(Debug, Clone, Default)]
pub struct ConsoleUpdate {
    pub access: Option<Access>,
    pub access_extra: Option<AccessExtra>,
    pub icon: Option<u16>,
    pub name: Option<Box<str>>,
}
