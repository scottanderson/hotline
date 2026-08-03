//! Field tags, split by direction: [`ClientFieldId`] for fields a client
//! sends, [`ServerFieldId`] for fields a server sends.
//!
//! Each type only has variants for tags that direction actually uses, so
//! `ServerFieldId::Text` and `ClientFieldId::Text` are distinct values
//! with distinct [`ClientFieldId::name`]/[`ServerFieldId::name`]
//! results, and a tag that's one-directional (`ErrorText` is
//! server-only, `Options` is client-only, etc.) simply isn't a valid
//! value of the type that never sends it.
//!
//! Variant names track the tail of the original wire constant (e.g.
//! `HTLS_DATA_SERVERNAME` -> `Servername`) rather than being split into
//! idiomatic multi-word Rust names, so a variant is always one grep away
//! from its source.

/// Field tags as they appear in a **client**-directed message. A
/// distinct type from [`ServerFieldId`], even where the same numeric tag
/// is used in both directions (e.g. `Name` is `102` on both sides).
#[repr(u16)]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClientFieldId {
    /// Generic payload: chat text, an instant message body, a news post, or the server agreement, depending on the enclosing transaction.
    ///
    /// Original: `HTLC_DATA_TEXT`.
    Text = 101,
    /// A user's display name.
    ///
    /// Original: `HTLC_DATA_NAME`.
    Name = 102,
    /// Socket/user ID.
    ///
    /// Original: `HTLC_DATA_UID`.
    Uid = 103,
    /// Icon ID.
    ///
    /// Original: `HTLC_DATA_ICON`.
    Icon = 104,
    /// Account login (XOR-obfuscated on the wire).
    ///
    /// Original: `HTLC_DATA_LOGIN`.
    Login = 105,
    /// Account password (XOR-obfuscated on the wire).
    ///
    /// Original: `HTLC_DATA_PASSWORD`.
    Password = 106,
    /// File transfer size in bytes.
    ///
    /// Original: `HTLC_DATA_HTXF_SIZE`.
    TransferSize = 108,
    /// Chat message style/formatting bitmap.
    ///
    /// Original: `HTLC_DATA_STYLE`.
    Style = 109,
    /// 64-bit access privilege bitmap. See `AccessPrivileges`.
    ///
    /// Original: `HTLC_DATA_ACCESS`.
    Access = 110,
    /// Contextual options bitmap; also reused as a kick-to-ban flag.
    ///
    /// Original: `HTLC_DATA_BAN`.
    Options = 113,
    /// Private chat room ID.
    ///
    /// Original: `HTLC_DATA_CHAT_ID`.
    ChatId = 114,
    /// Chat room subject string.
    ///
    /// Original: `HTLC_DATA_CHAT_SUBJECT`.
    ChatSubject = 115,
    /// Protocol version: the client's own version when client-sent, the server's version when server-sent.
    ///
    /// Original: `HTLC_DATA_CLIENTVERSION`.
    Version = 160,
    /// A file or folder's name.
    ///
    /// Original: `HTLC_DATA_FILE_NAME`.
    FileName = 201,
    /// A directory path.
    ///
    /// Original: `HTLC_DATA_DIR`.
    FilePath = 202,
    /// Resume-transfer descriptor.
    ///
    /// Original: `HTLC_DATA_RFLT`.
    ResumeFileTransfer = 203,
    /// File transfer options; also used for image preview requests.
    ///
    /// Original: `HTLC_DATA_FILE_PREVIEW`.
    FileXferOptions = 204,
    /// File/folder comment text.
    ///
    /// Original: `HTLC_DATA_FILE_COMMENT`.
    FileComment = 210,
    /// Requested new name for a rename.
    ///
    /// Original: `HTLC_DATA_FILE_RENAME`.
    FileNewName = 211,
    /// Requested destination path for a move/alias.
    ///
    /// Original: `HTLC_DATA_DIR_RENAME`.
    FileNewPath = 212,
    /// Number of items in a folder.
    ///
    /// Original: `HTLC_DATA_FILE_NFILES`.
    FolderItemCount = 220,
    /// Packed news category/folder listing entry (pre-1.5).
    ///
    /// Original: `HTLC_DATA_NEWS_DIRLIST`.
    NewsCatListData = 320,
    /// Packed news article listing entry.
    ///
    /// Original: `HTLC_DATA_NEWS_CATLIST`.
    NewsArtListData = 321,
    /// A news category's name.
    ///
    /// Original: `HTLC_DATA_NEWS_CATNAME`.
    NewsCatName = 322,
    /// A news folder/category path.
    ///
    /// Original: `HTLC_DATA_NEWS_DIR`.
    NewsPath = 325,
    /// News article/thread ID.
    ///
    /// Original: `HTLC_DATA_NEWS_THREADID`.
    NewsArtId = 326,
    /// News article MIME type, e.g. "text/plain".
    ///
    /// Original: `HTLC_DATA_NEWS_MIMETYPE`.
    NewsArtDataFlavor = 327,
    /// News article subject/title.
    ///
    /// Original: `HTLC_DATA_NEWS_SUBJECT`.
    NewsArtTitle = 328,
    /// News article author name.
    ///
    /// Original: `HTLC_DATA_NEWS_POSTER`.
    NewsArtPoster = 329,
    /// News article post date.
    ///
    /// Original: `HTLC_DATA_NEWS_DATE`.
    NewsArtDate = 330,
    /// Previous article ID in thread order.
    ///
    /// Original: `HTLC_DATA_NEWS_PREVTHREADID`.
    NewsArtPrevArt = 331,
    /// Next article ID in thread order.
    ///
    /// Original: `HTLC_DATA_NEWS_NEXTTHREADID`.
    NewsArtNextArt = 332,
    /// News article body text.
    ///
    /// Original: `HTLC_DATA_NEWS_POST`.
    NewsArtData = 333,
    /// Parent article ID.
    ///
    /// Original: `HTLC_DATA_NEWS_PARENTTHREADID`.
    NewsArtParentArt = 335,
    /// First child article ID.
    ///
    /// Original: `HTLC_DATA_NEWS_NEXTSUBTHREADID`.
    NewsArt1stChildArt = 336,
    /// Whether deleting this article recursively deletes replies.
    ///
    /// Original: `HTLC_DATA_NEWS_DELETEREPLIES`.
    NewsArtRecurseDel = 337,
}

impl ClientFieldId {
    /// Look up the variant for a raw wire value, if any client field
    /// uses it.
    pub const fn from_u16(id: u16) -> Option<Self> {
        Some(match id {
            101 => Self::Text,
            102 => Self::Name,
            103 => Self::Uid,
            104 => Self::Icon,
            105 => Self::Login,
            106 => Self::Password,
            108 => Self::TransferSize,
            109 => Self::Style,
            110 => Self::Access,
            113 => Self::Options,
            114 => Self::ChatId,
            115 => Self::ChatSubject,
            160 => Self::Version,
            201 => Self::FileName,
            202 => Self::FilePath,
            203 => Self::ResumeFileTransfer,
            204 => Self::FileXferOptions,
            210 => Self::FileComment,
            211 => Self::FileNewName,
            212 => Self::FileNewPath,
            220 => Self::FolderItemCount,
            320 => Self::NewsCatListData,
            321 => Self::NewsArtListData,
            322 => Self::NewsCatName,
            325 => Self::NewsPath,
            326 => Self::NewsArtId,
            327 => Self::NewsArtDataFlavor,
            328 => Self::NewsArtTitle,
            329 => Self::NewsArtPoster,
            330 => Self::NewsArtDate,
            331 => Self::NewsArtPrevArt,
            332 => Self::NewsArtNextArt,
            333 => Self::NewsArtData,
            335 => Self::NewsArtParentArt,
            336 => Self::NewsArt1stChildArt,
            337 => Self::NewsArtRecurseDel,
            _ => return None,
        })
    }

    /// The wire value.
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    /// The original constant name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "HTLC_DATA_TEXT",
            Self::Name => "HTLC_DATA_NAME",
            Self::Uid => "HTLC_DATA_UID",
            Self::Icon => "HTLC_DATA_ICON",
            Self::Login => "HTLC_DATA_LOGIN",
            Self::Password => "HTLC_DATA_PASSWORD",
            Self::TransferSize => "HTLC_DATA_HTXF_SIZE",
            Self::Style => "HTLC_DATA_STYLE",
            Self::Access => "HTLC_DATA_ACCESS",
            Self::Options => "HTLC_DATA_BAN",
            Self::ChatId => "HTLC_DATA_CHAT_ID",
            Self::ChatSubject => "HTLC_DATA_CHAT_SUBJECT",
            Self::Version => "HTLC_DATA_CLIENTVERSION",
            Self::FileName => "HTLC_DATA_FILE_NAME",
            Self::FilePath => "HTLC_DATA_DIR",
            Self::ResumeFileTransfer => "HTLC_DATA_RFLT",
            Self::FileXferOptions => "HTLC_DATA_FILE_PREVIEW",
            Self::FileComment => "HTLC_DATA_FILE_COMMENT",
            Self::FileNewName => "HTLC_DATA_FILE_RENAME",
            Self::FileNewPath => "HTLC_DATA_DIR_RENAME",
            Self::FolderItemCount => "HTLC_DATA_FILE_NFILES",
            Self::NewsCatListData => "HTLC_DATA_NEWS_DIRLIST",
            Self::NewsArtListData => "HTLC_DATA_NEWS_CATLIST",
            Self::NewsCatName => "HTLC_DATA_NEWS_CATNAME",
            Self::NewsPath => "HTLC_DATA_NEWS_DIR",
            Self::NewsArtId => "HTLC_DATA_NEWS_THREADID",
            Self::NewsArtDataFlavor => "HTLC_DATA_NEWS_MIMETYPE",
            Self::NewsArtTitle => "HTLC_DATA_NEWS_SUBJECT",
            Self::NewsArtPoster => "HTLC_DATA_NEWS_POSTER",
            Self::NewsArtDate => "HTLC_DATA_NEWS_DATE",
            Self::NewsArtPrevArt => "HTLC_DATA_NEWS_PREVTHREADID",
            Self::NewsArtNextArt => "HTLC_DATA_NEWS_NEXTTHREADID",
            Self::NewsArtData => "HTLC_DATA_NEWS_POST",
            Self::NewsArtParentArt => "HTLC_DATA_NEWS_PARENTTHREADID",
            Self::NewsArt1stChildArt => "HTLC_DATA_NEWS_NEXTSUBTHREADID",
            Self::NewsArtRecurseDel => "HTLC_DATA_NEWS_DELETEREPLIES",
        }
    }

    /// The original constant name for a raw wire value, known or not.
    pub const fn name_for_raw(id: u16) -> &'static str {
        match Self::from_u16(id) {
            Some(known) => known.name(),
            None => "(unrecognized field id)",
        }
    }
}

impl From<ClientFieldId> for u16 {
    fn from(id: ClientFieldId) -> u16 {
        id.as_u16()
    }
}

/// Field tags as they appear in a **server**-directed message.
///
/// See [`ClientFieldId`] for why this is a separate type rather than a
/// shared one.
#[repr(u16)]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServerFieldId {
    /// Error message text.
    ///
    /// Original: `HTLS_DATA_TASKERROR`.
    ErrorText = 100,
    /// Generic payload: chat text, an instant message body, a news post, or the server agreement, depending on the enclosing transaction.
    ///
    /// Original: `HTLS_DATA_TEXT`.
    Text = 101,
    /// A user's display name.
    ///
    /// Original: `HTLS_DATA_NAME`.
    Name = 102,
    /// Socket/user ID.
    ///
    /// Original: `HTLS_DATA_UID`.
    UserId = 103,
    /// Icon ID.
    ///
    /// Original: `HTLS_DATA_ICON`.
    Icon = 104,
    /// Account login (XOR-obfuscated on the wire).
    ///
    /// Original: `HTLS_DATA_LOGIN`.
    Login = 105,
    /// Account password (XOR-obfuscated on the wire).
    ///
    /// Original: `HTLS_DATA_PASSWORD`.
    Password = 106,
    /// File transfer reference number, assigned by the server.
    ///
    /// Original: `HTLS_DATA_HTXF_REF`.
    TransferId = 107,
    /// File transfer size in bytes.
    ///
    /// Original: `HTLS_DATA_HTXF_SIZE`.
    TransferSize = 108,
    /// Chat message style/formatting bitmap.
    ///
    /// Original: `HTLS_DATA_STYLE`.
    Style = 109,
    /// 64-bit access privilege bitmap. See `AccessPrivileges`.
    ///
    /// Original: `HTLS_DATA_ACCESS`.
    Access = 110,
    /// User status bitmap (away, admin, etc) / legacy color.
    ///
    /// Original: `HTLS_DATA_COLOR`.
    Color = 112,
    /// Private chat room ID.
    ///
    /// Original: `HTLS_DATA_CHAT_ID`.
    ChatId = 114,
    /// Chat room subject string.
    ///
    /// Original: `HTLS_DATA_CHAT_SUBJECT`.
    ChatSubject = 115,
    /// Position in the download queue.
    ///
    /// Original: `HTLS_DATA_QUEUE_POSITION`.
    QueuePosition = 116,
    /// Raw banner image bytes.
    ///
    /// Original: `HTLS_DATA_BANNER`.
    Banner = 151,
    /// Banner format tag (URL/JPEG/GIF/BMP/PICT).
    ///
    /// Original: `HTLS_DATA_BANNER_TYPE`.
    BannerType = 152,
    /// Banner URL, when the type is URL.
    ///
    /// Original: `HTLS_DATA_BANNER_URL`.
    BannerUrl = 153,
    /// Set when the server has no agreement to show.
    ///
    /// Original: `HTLS_DATA_NOAGREEMENT`.
    Noagreement = 154,
    /// Protocol version: the client's own version when client-sent, the server's version when server-sent.
    ///
    /// Original: `HTLS_DATA_SERVERVERSION`.
    Serverversion = 160,
    /// Banner ID used to fetch the banner over HTTP.
    ///
    /// Original: `HTLS_DATA_BANNERID`.
    Bannerid = 161,
    /// Server's display name.
    ///
    /// Original: `HTLS_DATA_SERVERNAME`.
    Servername = 162,
    /// A packed file-listing entry.
    ///
    /// Original: `HTLS_DATA_FILE_LIST`.
    FileList = 200,
    /// A file or folder's name.
    ///
    /// Original: `HTLS_DATA_FILE_NAME`.
    FileName = 201,
    /// Resume-transfer descriptor.
    ///
    /// Original: `HTLS_DATA_RFLT`.
    ResumeFileTransfer = 203,
    /// Mac file type as a 4-character string.
    ///
    /// Original: `HTLS_DATA_FILE_TYPE`.
    FileType = 205,
    /// Mac file creator as a 4-character string.
    ///
    /// Original: `HTLS_DATA_FILE_CREATOR`.
    FileCreator = 206,
    /// File size in bytes.
    ///
    /// Original: `HTLS_DATA_FILE_SIZE`.
    FileSize = 207,
    /// File creation date.
    ///
    /// Original: `HTLS_DATA_FILE_DATE_CREATE`.
    FileDateCreate = 208,
    /// File modification date.
    ///
    /// Original: `HTLS_DATA_FILE_DATE_MODIFY`.
    FileDateModify = 209,
    /// File/folder comment text.
    ///
    /// Original: `HTLS_DATA_FILE_COMMENT`.
    FileComment = 210,
    /// 4-character Mac type code, packed.
    ///
    /// Original: `HTLS_DATA_FILE_ICON`.
    FileIcon = 213,
    /// Number of items in a folder.
    ///
    /// Original: `HTLS_DATA_FILE_NFILES`.
    FileCount = 220,
    /// A packed user-list entry.
    ///
    /// Original: `HTLS_DATA_USER_LIST`.
    UserList = 300,
    /// Packed news category/folder listing entry (pre-1.5).
    ///
    /// Original: `HTLS_DATA_NEWS_DIRLIST`.
    NewsDirlist = 320,
    /// Packed news article listing entry.
    ///
    /// Original: `HTLS_DATA_NEWS_CATLIST`.
    NewsCatlist = 321,
    /// Packed news category/folder listing entry (1.5+).
    ///
    /// Original: `HTLS_DATA_NEWS_DIRLIST_EXTENDED`.
    NewsDirlistExtended = 323,
    /// A news folder/category path.
    ///
    /// Original: `HTLS_DATA_NEWS_DIR`.
    NewsDir = 325,
    /// News article/thread ID.
    ///
    /// Original: `HTLS_DATA_NEWS_THREADID`.
    NewsThreadid = 326,
    /// News article MIME type, e.g. "text/plain".
    ///
    /// Original: `HTLS_DATA_NEWS_MIMETYPE`.
    NewsMimetype = 327,
    /// News article subject/title.
    ///
    /// Original: `HTLS_DATA_NEWS_SUBJECT`.
    NewsSubject = 328,
    /// News article author name.
    ///
    /// Original: `HTLS_DATA_NEWS_POSTER`.
    NewsPoster = 329,
    /// News article post date.
    ///
    /// Original: `HTLS_DATA_NEWS_DATE`.
    NewsDate = 330,
    /// Previous article ID in thread order.
    ///
    /// Original: `HTLS_DATA_NEWS_PREVTHREADID`.
    NewsPrevthreadid = 331,
    /// Next article ID in thread order.
    ///
    /// Original: `HTLS_DATA_NEWS_NEXTTHREADID`.
    NewsNextthreadid = 332,
    /// News article body text.
    ///
    /// Original: `HTLS_DATA_NEWS_POST`.
    NewsPost = 333,
    /// Parent article ID.
    ///
    /// Original: `HTLS_DATA_NEWS_PARENTTHREADID`.
    NewsParentthreadid = 335,
    /// First child article ID.
    ///
    /// Original: `HTLS_DATA_NEWS_NEXTSUBTHREADID`.
    NewsNextsubthreadid = 336,
}

impl ServerFieldId {
    /// Look up the variant for a raw wire value, if any server field
    /// uses it.
    pub const fn from_u16(id: u16) -> Option<Self> {
        Some(match id {
            100 => Self::ErrorText,
            101 => Self::Text,
            102 => Self::Name,
            103 => Self::UserId,
            104 => Self::Icon,
            105 => Self::Login,
            106 => Self::Password,
            107 => Self::TransferId,
            108 => Self::TransferSize,
            109 => Self::Style,
            110 => Self::Access,
            112 => Self::Color,
            114 => Self::ChatId,
            115 => Self::ChatSubject,
            116 => Self::QueuePosition,
            151 => Self::Banner,
            152 => Self::BannerType,
            153 => Self::BannerUrl,
            154 => Self::Noagreement,
            160 => Self::Serverversion,
            161 => Self::Bannerid,
            162 => Self::Servername,
            200 => Self::FileList,
            201 => Self::FileName,
            203 => Self::ResumeFileTransfer,
            205 => Self::FileType,
            206 => Self::FileCreator,
            207 => Self::FileSize,
            208 => Self::FileDateCreate,
            209 => Self::FileDateModify,
            210 => Self::FileComment,
            213 => Self::FileIcon,
            220 => Self::FileCount,
            300 => Self::UserList,
            320 => Self::NewsDirlist,
            321 => Self::NewsCatlist,
            323 => Self::NewsDirlistExtended,
            325 => Self::NewsDir,
            326 => Self::NewsThreadid,
            327 => Self::NewsMimetype,
            328 => Self::NewsSubject,
            329 => Self::NewsPoster,
            330 => Self::NewsDate,
            331 => Self::NewsPrevthreadid,
            332 => Self::NewsNextthreadid,
            333 => Self::NewsPost,
            335 => Self::NewsParentthreadid,
            336 => Self::NewsNextsubthreadid,
            _ => return None,
        })
    }

    /// The wire value.
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    /// The original constant name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ErrorText => "HTLS_DATA_TASKERROR",
            Self::Text => "HTLS_DATA_TEXT",
            Self::Name => "HTLS_DATA_NAME",
            Self::UserId => "HTLS_DATA_UID",
            Self::Icon => "HTLS_DATA_ICON",
            Self::Login => "HTLS_DATA_LOGIN",
            Self::Password => "HTLS_DATA_PASSWORD",
            Self::TransferId => "HTLS_DATA_HTXF_REF",
            Self::TransferSize => "HTLS_DATA_HTXF_SIZE",
            Self::Style => "HTLS_DATA_STYLE",
            Self::Access => "HTLS_DATA_ACCESS",
            Self::Color => "HTLS_DATA_COLOR",
            Self::ChatId => "HTLS_DATA_CHAT_ID",
            Self::ChatSubject => "HTLS_DATA_CHAT_SUBJECT",
            Self::QueuePosition => "HTLS_DATA_QUEUE_POSITION",
            Self::Banner => "HTLS_DATA_BANNER",
            Self::BannerType => "HTLS_DATA_BANNER_TYPE",
            Self::BannerUrl => "HTLS_DATA_BANNER_URL",
            Self::Noagreement => "HTLS_DATA_NOAGREEMENT",
            Self::Serverversion => "HTLS_DATA_SERVERVERSION",
            Self::Bannerid => "HTLS_DATA_BANNERID",
            Self::Servername => "HTLS_DATA_SERVERNAME",
            Self::FileList => "HTLS_DATA_FILE_LIST",
            Self::FileName => "HTLS_DATA_FILE_NAME",
            Self::ResumeFileTransfer => "HTLS_DATA_RFLT",
            Self::FileType => "HTLS_DATA_FILE_TYPE",
            Self::FileCreator => "HTLS_DATA_FILE_CREATOR",
            Self::FileSize => "HTLS_DATA_FILE_SIZE",
            Self::FileDateCreate => "HTLS_DATA_FILE_DATE_CREATE",
            Self::FileDateModify => "HTLS_DATA_FILE_DATE_MODIFY",
            Self::FileComment => "HTLS_DATA_FILE_COMMENT",
            Self::FileIcon => "HTLS_DATA_FILE_ICON",
            Self::FileCount => "HTLS_DATA_FILE_NFILES",
            Self::UserList => "HTLS_DATA_USER_LIST",
            Self::NewsDirlist => "HTLS_DATA_NEWS_DIRLIST",
            Self::NewsCatlist => "HTLS_DATA_NEWS_CATLIST",
            Self::NewsDirlistExtended => "HTLS_DATA_NEWS_DIRLIST_EXTENDED",
            Self::NewsDir => "HTLS_DATA_NEWS_DIR",
            Self::NewsThreadid => "HTLS_DATA_NEWS_THREADID",
            Self::NewsMimetype => "HTLS_DATA_NEWS_MIMETYPE",
            Self::NewsSubject => "HTLS_DATA_NEWS_SUBJECT",
            Self::NewsPoster => "HTLS_DATA_NEWS_POSTER",
            Self::NewsDate => "HTLS_DATA_NEWS_DATE",
            Self::NewsPrevthreadid => "HTLS_DATA_NEWS_PREVTHREADID",
            Self::NewsNextthreadid => "HTLS_DATA_NEWS_NEXTTHREADID",
            Self::NewsPost => "HTLS_DATA_NEWS_POST",
            Self::NewsParentthreadid => "HTLS_DATA_NEWS_PARENTTHREADID",
            Self::NewsNextsubthreadid => "HTLS_DATA_NEWS_NEXTSUBTHREADID",
        }
    }

    /// The original constant name for a raw wire value, known or not.
    pub const fn name_for_raw(id: u16) -> &'static str {
        match Self::from_u16(id) {
            Some(known) => known.name(),
            None => "(unrecognized field id)",
        }
    }
}

impl From<ServerFieldId> for u16 {
    fn from(id: ServerFieldId) -> u16 {
        id.as_u16()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_and_server_field_ids_never_collide_in_name() {
        assert_ne!(ClientFieldId::Name.name(), ServerFieldId::Name.name());
    }

    #[test]
    fn from_u16_round_trips() {
        assert_eq!(ClientFieldId::from_u16(102), Some(ClientFieldId::Name));
        assert_eq!(u16::from(ClientFieldId::Name), 102);
        assert_eq!(ClientFieldId::from_u16(65535), None);
    }

    #[test]
    fn name_for_raw_handles_unknown_values() {
        assert_eq!(
            ClientFieldId::name_for_raw(65535),
            "(unrecognized field id)"
        );
        assert_eq!(ClientFieldId::name_for_raw(102), "HTLC_DATA_NAME");
    }

    #[test]
    fn file_icon_213_does_not_collide_with_file_type_205() {
        assert_eq!(ServerFieldId::from_u16(213), Some(ServerFieldId::FileIcon));
        assert_eq!(ServerFieldId::from_u16(205), Some(ServerFieldId::FileType));
    }
}
