//! The key table: each key, where it works, and what it does. The dispatch
//! and the hint line read the same table.

use std::borrow::Cow;
use std::cmp::Reverse;

use hotline_proto::Access;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustc_hash::{FxHashMap, FxHashSet};

use super::is_modified_char;
use super::modal::ModalKind;

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    /// Disconnect a user without a ban.
    DisconnectUser,
    /// Disconnect a user, and ban the address for a while.
    TemporaryBan,
    /// Disconnect a user, and ban the address until an administrator lifts the ban.
    PermanentBan,
    /// Delete the item that the box names.
    Delete,
    /// Write a reply to an article.
    Reply,
    /// Join a private chat room.
    Join,
    /// Connect again to the last server.
    Reconnect,
    /// Open the Connect to form.
    ConnectTo,
    /// Move between the title and the text of an article.
    SwitchField,
    /// Send a message.
    Send,
    /// Upload the path in the box.
    Upload,
    /// Save the text in the box.
    Save,
    /// Set the subject of a chat room.
    SetSubject,
    /// Post an article.
    Post,
    /// Create a news bundle or category.
    Create,
    /// Open the account with the login in the box.
    OpenAccount,
    /// Start a new line in a message.
    NewLine,
    /// Quit `hlc`.
    Quit,
    /// Close a box that only shows text.
    Close,
    /// Close a box, and do not do what it asks.
    Cancel,
    /// Refuse an invitation to a private chat room.
    Decline,
}

impl Intent {
    /// Each intent, in hint order.
    pub const ALL: &[Self] = &[
        Self::DisconnectUser,
        Self::TemporaryBan,
        Self::PermanentBan,
        Self::Delete,
        Self::Reply,
        Self::Join,
        Self::Reconnect,
        Self::ConnectTo,
        Self::SwitchField,
        Self::Send,
        Self::Upload,
        Self::Save,
        Self::SetSubject,
        Self::Post,
        Self::Create,
        Self::OpenAccount,
        Self::NewLine,
        Self::Quit,
        Self::Close,
        Self::Cancel,
        Self::Decline,
    ];

    /// The text of the hint.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DisconnectUser => "disconnect",
            Self::TemporaryBan => "temporary ban",
            Self::PermanentBan => "permanent ban",
            Self::Delete => "delete",
            Self::Reply => "reply",
            Self::Join => "join",
            Self::Reconnect => "reconnect",
            Self::ConnectTo => "connect to",
            Self::SwitchField => "title or text",
            Self::Send => "send",
            Self::Upload => "upload",
            Self::Save => "save",
            Self::SetSubject => "set",
            Self::Post => "post",
            Self::Create => "create",
            Self::OpenAccount => "open",
            Self::NewLine => "new line",
            Self::Quit => "quit",
            Self::Close => "close",
            Self::Cancel => "cancel",
            Self::Decline => "decline",
        }
    }

    /// The privileges, any one of which turns the intent on. Empty for an
    /// intent that needs none.
    pub const fn enabled_by(self) -> Access {
        match self {
            Self::Reply => Access::POST_NEWS_ART,
            _ => Access::empty(),
        }
    }
}

/// Where a binding works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Modal(ModalKind),
}

/// A key and the modifiers that it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    /// `code` with no modifier.
    pub const fn plain(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }

    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// `true` if `key` presses this chord. A chord with no modifier takes
    /// a character only without Ctrl and Alt, and any other key with any
    /// modifier.
    pub fn matches(self, key: KeyEvent) -> bool {
        key.code == self.code
            && if self.modifiers.is_empty() {
                !is_modified_char(key)
            } else {
                key.modifiers.contains(self.modifiers)
            }
    }

    /// The chord of the next key with the same modifiers, as `F2` after
    /// `F1`.
    fn successor(self) -> Option<Self> {
        let code = match self.code {
            KeyCode::F(n) => KeyCode::F(n.checked_add(1)?),
            KeyCode::Char(c) => {
                KeyCode::Char(u32::from(c).checked_add(1).and_then(char::from_u32)?)
            }
            _ => return None,
        };
        Some(Self {
            code,
            modifiers: self.modifiers,
        })
    }

    /// The chord as a hint shows it, such as `Esc`, `y`, `⇧Enter`, or `^J`.
    pub fn label(self) -> Cow<'static, str> {
        let key = match self.code {
            KeyCode::Char(c) if self.modifiers.contains(KeyModifiers::CONTROL) => {
                c.to_ascii_uppercase().to_string().into()
            }
            KeyCode::Char(c) => c.to_string().into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Enter => "Enter".into(),
            code => code.to_string().into(),
        };
        let mut label: String = [
            (KeyModifiers::ALT, "Alt-"),
            (KeyModifiers::CONTROL, "^"),
            (KeyModifiers::SHIFT, "⇧"),
        ]
        .into_iter()
        .filter(|&(modifier, _)| self.modifiers.contains(modifier))
        .map(|(_, prefix)| prefix)
        .collect();
        if label.is_empty() {
            return key;
        }
        label.push_str(&key);
        label.into()
    }
}

/// The bindings of keys to intents.
#[derive(Debug)]
pub struct Keymap {
    bindings: Vec<(Scope, KeyChord, Intent)>,
    /// The label of each chord.
    labels: FxHashMap<KeyChord, Cow<'static, str>>,
    /// The chords of each intent in each scope, in the order of `bindings`.
    chords: FxHashMap<(Scope, Intent), Vec<KeyChord>>,
    /// Each chord that each scope binds.
    bound: FxHashSet<(Scope, KeyChord)>,
}

impl Default for Keymap {
    fn default() -> Self {
        const fn key(code: KeyCode, intent: Intent) -> (KeyChord, Intent) {
            (KeyChord::plain(code), intent)
        }
        const fn chord(
            code: KeyCode,
            modifiers: KeyModifiers,
            intent: Intent,
        ) -> (KeyChord, Intent) {
            (KeyChord::new(code, modifiers), intent)
        }
        const CANCEL: (KeyChord, Intent) = key(KeyCode::Esc, Intent::Cancel);
        const CLOSE: &[(KeyChord, Intent)] = &[
            key(KeyCode::Esc, Intent::Close),
            key(KeyCode::Enter, Intent::Close),
        ];
        const DELETE: &[(KeyChord, Intent)] = &[key(KeyCode::Char('y'), Intent::Delete), CANCEL];
        const NEW_LINE: [(KeyChord, Intent); 3] = [
            chord(KeyCode::Enter, KeyModifiers::SHIFT, Intent::NewLine),
            chord(KeyCode::Enter, KeyModifiers::ALT, Intent::NewLine),
            chord(KeyCode::Char('j'), KeyModifiers::CONTROL, Intent::NewLine),
        ];
        const MESSAGE: &[(KeyChord, Intent)] = &[
            key(KeyCode::Enter, Intent::Send),
            NEW_LINE[0],
            NEW_LINE[1],
            NEW_LINE[2],
            CANCEL,
        ];
        const SAVE: &[(KeyChord, Intent)] = &[key(KeyCode::Enter, Intent::Save), CANCEL];
        let modals: [(ModalKind, &[(KeyChord, Intent)]); 19] = [
            (ModalKind::Info, CLOSE),
            (ModalKind::FileInfo, CLOSE),
            (
                ModalKind::Quit,
                &[key(KeyCode::Char('y'), Intent::Quit), CANCEL],
            ),
            (
                ModalKind::Disconnect,
                &[
                    key(KeyCode::Char('d'), Intent::DisconnectUser),
                    key(KeyCode::Char('t'), Intent::TemporaryBan),
                    key(KeyCode::Char('p'), Intent::PermanentBan),
                    CANCEL,
                ],
            ),
            (ModalKind::DeleteFile, DELETE),
            (ModalKind::DeleteNews, DELETE),
            (ModalKind::DeleteAccount, DELETE),
            (
                ModalKind::Disconnected,
                &[
                    key(KeyCode::Char('r'), Intent::Reconnect),
                    key(KeyCode::Char('c'), Intent::ConnectTo),
                    key(KeyCode::Char('q'), Intent::Quit),
                    key(KeyCode::Esc, Intent::Close),
                    key(KeyCode::Enter, Intent::Close),
                ],
            ),
            (
                ModalKind::Invite,
                &[
                    key(KeyCode::Enter, Intent::Join),
                    key(KeyCode::Esc, Intent::Decline),
                ],
            ),
            (
                ModalKind::Article,
                &[
                    key(KeyCode::Char('r'), Intent::Reply),
                    key(KeyCode::Esc, Intent::Close),
                    key(KeyCode::Enter, Intent::Close),
                ],
            ),
            (ModalKind::InstantMsg, MESSAGE),
            (ModalKind::Broadcast, MESSAGE),
            (
                ModalKind::Upload,
                &[key(KeyCode::Enter, Intent::Upload), CANCEL],
            ),
            (ModalKind::SaveAs, SAVE),
            (ModalKind::FileEdit, SAVE),
            (
                ModalKind::Subject,
                &[key(KeyCode::Enter, Intent::SetSubject), CANCEL],
            ),
            (
                ModalKind::Post,
                &[
                    key(KeyCode::Tab, Intent::SwitchField),
                    key(KeyCode::Enter, Intent::Post),
                    NEW_LINE[0],
                    NEW_LINE[1],
                    NEW_LINE[2],
                    CANCEL,
                ],
            ),
            (
                ModalKind::NewsName,
                &[key(KeyCode::Enter, Intent::Create), CANCEL],
            ),
            (
                ModalKind::OpenAccount,
                &[key(KeyCode::Enter, Intent::OpenAccount), CANCEL],
            ),
        ];
        Self::new(
            modals
                .into_iter()
                .flat_map(|(kind, keys)| {
                    keys.iter()
                        .map(move |&(chord, intent)| (Scope::Modal(kind), chord, intent))
                })
                .collect(),
        )
    }
}

impl Keymap {
    /// The key table of `bindings`, with the label of each chord and an
    /// index of the chords.
    pub(super) fn new(bindings: Vec<(Scope, KeyChord, Intent)>) -> Self {
        let labels = bindings
            .iter()
            .map(|&(_, chord, _)| (chord, chord.label()))
            .collect();
        let mut chords: FxHashMap<_, Vec<_>> = FxHashMap::default();
        for &(scope, chord, intent) in &bindings {
            chords.entry((scope, intent)).or_default().push(chord);
        }
        let bound = bindings
            .iter()
            .map(|&(scope, chord, _)| (scope, chord))
            .collect();
        Self {
            bindings,
            labels,
            chords,
            bound,
        }
    }

    /// The label of `chord`, or `""` for a chord that the table does not bind.
    pub fn label(&self, chord: KeyChord) -> &str {
        self.labels.get(&chord).map_or("", AsRef::as_ref)
    }

    /// The keys of a hint for `chords`: a range of three or more keys in a
    /// row, as in `F1-F9`, or each key with `/` between, as in `Esc/Enter`.
    pub fn hint_key(&self, chords: impl Iterator<Item = KeyChord> + Clone) -> Cow<'_, str> {
        let mut rest = chords.clone();
        let Some(first) = rest.next() else {
            return "".into();
        };
        let (count, last, in_a_row) =
            rest.fold((1_usize, first, true), |(count, prev, in_a_row), chord| {
                (
                    count.saturating_add(1),
                    chord,
                    in_a_row && prev.successor() == Some(chord),
                )
            });
        match count {
            1 => self.label(first).into(),
            3.. if in_a_row => format!("{}-{}", self.label(first), self.label(last)).into(),
            _ => chords
                .map(|chord| self.label(chord))
                .collect::<Vec<_>>()
                .join("/")
                .into(),
        }
    }

    /// The intent of `key` in the first scope of `scopes` that binds it. The
    /// chord with the most modifiers wins, as `⇧Enter` over `Enter`.
    pub fn intent(&self, scopes: &[Scope], key: KeyEvent) -> Option<Intent> {
        scopes.iter().find_map(|&scope| {
            self.bindings
                .iter()
                .filter(|&&(s, chord, _)| s == scope && chord.matches(key))
                .min_by_key(|&&(_, chord, _)| Reverse(chord.modifiers.bits().count_ones()))
                .map(|&(_, _, intent)| intent)
        })
    }

    /// The chords that reach `intent` from `scopes`. A chord that a nearer
    /// scope binds to another intent does not reach it.
    pub fn chords(
        &self,
        scopes: &[Scope],
        intent: Intent,
    ) -> impl Iterator<Item = KeyChord> + Clone {
        scopes.iter().flat_map(move |&scope| {
            self.chords_in(scope, intent)
                .filter(move |&chord| self.first_scope(scopes, chord) == Some(scope))
        })
    }

    /// The chords that bind `intent` in `scope`.
    fn chords_in(&self, scope: Scope, intent: Intent) -> impl Iterator<Item = KeyChord> + Clone {
        self.chords
            .get(&(scope, intent))
            .into_iter()
            .flatten()
            .copied()
    }

    /// The first scope of `scopes` that binds `chord`.
    fn first_scope(&self, scopes: &[Scope], chord: KeyChord) -> Option<Scope> {
        scopes
            .iter()
            .copied()
            .find(|&scope| self.bound.contains(&(scope, chord)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: Scope = Scope::Modal(ModalKind::Info);
    const QUIT: Scope = Scope::Modal(ModalKind::Quit);

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn matches_refuses_a_character_with_ctrl_or_alt() {
        let y = KeyChord::plain(KeyCode::Char('y'));
        assert!(y.matches(key(KeyCode::Char('y'), KeyModifiers::NONE)));
        assert!(!y.matches(key(KeyCode::Char('y'), KeyModifiers::CONTROL)));
        assert!(!y.matches(key(KeyCode::Char('y'), KeyModifiers::ALT)));
    }

    #[test]
    fn matches_takes_another_key_with_any_modifier() {
        let esc = KeyChord::plain(KeyCode::Esc);
        assert!(esc.matches(key(KeyCode::Esc, KeyModifiers::SHIFT)));
    }

    #[test]
    fn intent_takes_the_first_scope_that_binds_the_key() {
        let keymap = Keymap::default();
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(keymap.intent(&[QUIT, INFO], esc), Some(Intent::Cancel));
        assert_eq!(keymap.intent(&[INFO, QUIT], esc), Some(Intent::Close));
        let up = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(keymap.intent(&[QUIT], up), None);
    }

    #[test]
    fn intent_prefers_the_chord_with_more_modifiers() {
        let keymap = Keymap::default();
        let message = [Scope::Modal(ModalKind::InstantMsg)];
        let intent = |code, modifiers| keymap.intent(&message, key(code, modifiers));
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::NONE),
            Some(Intent::Send)
        );
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::SHIFT),
            Some(Intent::NewLine)
        );
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::ALT),
            Some(Intent::NewLine)
        );
        assert_eq!(
            intent(KeyCode::Char('j'), KeyModifiers::CONTROL),
            Some(Intent::NewLine)
        );
        assert_eq!(intent(KeyCode::Char('j'), KeyModifiers::NONE), None);
    }

    #[test]
    fn label_shows_the_modifiers() {
        let label = |code, modifiers| KeyChord::new(code, modifiers).label();
        assert_eq!(label(KeyCode::Enter, KeyModifiers::SHIFT), "⇧Enter");
        assert_eq!(label(KeyCode::Char('j'), KeyModifiers::CONTROL), "^J");
        assert_eq!(label(KeyCode::Char('t'), KeyModifiers::ALT), "Alt-t");
        assert_eq!(label(KeyCode::Tab, KeyModifiers::NONE), "Tab");
    }

    #[test]
    fn chords_leave_out_a_chord_that_a_nearer_scope_takes() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap
                .chords(&[QUIT, INFO], Intent::Close)
                .collect::<Vec<_>>(),
            [KeyChord::plain(KeyCode::Enter)]
        );
        assert_eq!(
            keymap.chords(&[INFO], Intent::Close).collect::<Vec<_>>(),
            [
                KeyChord::plain(KeyCode::Esc),
                KeyChord::plain(KeyCode::Enter)
            ]
        );
    }

    #[test]
    fn all_has_each_intent_of_the_default_table_once() {
        let missing: Vec<_> = Keymap::default()
            .bindings
            .iter()
            .map(|&(_, _, intent)| intent)
            .filter(|intent| !Intent::ALL.contains(intent))
            .collect();
        assert!(missing.is_empty(), "not in Intent::ALL: {missing:?}");
        let unique: FxHashSet<_> = Intent::ALL.iter().collect();
        assert_eq!(unique.len(), Intent::ALL.len());
    }

    #[test]
    fn hint_key_shows_a_range_only_for_three_or_more_keys_in_a_row() {
        let f = |n| KeyChord::plain(KeyCode::F(n));
        let keymap = Keymap::new([1, 2, 3, 5, 9].map(|n| (QUIT, f(n), Intent::Cancel)).into());
        let key = |numbers: &[u8]| keymap.hint_key(numbers.iter().map(|&n| f(n))).into_owned();
        assert_eq!(key(&[1]), "F1");
        assert_eq!(key(&[1, 2]), "F1/F2");
        assert_eq!(key(&[1, 2, 3]), "F1-F3");
        assert_eq!(key(&[1, 5, 9]), "F1/F5/F9");
        assert_eq!(key(&[1, 2, 3, 5]), "F1/F2/F3/F5");
    }
}
