//! The terminal bell, which rings on a private message, an invitation, a
//! finished transfer, and a chat line in a room tab that the user cannot see.

use std::io::{self, Write};

use hotline_proto::opcode::Opcode;

use crate::view::Tab;

bitflags::bitflags! {
    /// The events that ring the bell.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Bell: u8 {
        /// A chat line in the Chat tab that the user cannot see.
        const CHAT = 1 << 0;
        /// A chat line in a room tab that the user cannot see.
        const ROOM = 1 << 1;
        /// A classic news post that the user cannot see.
        const NEWS = 1 << 2;
        /// A private message, an administrator message, or a broadcast.
        const MESSAGE = 1 << 3;
        /// An invitation to a private chat.
        const INVITE = 1 << 4;
        /// A finished file transfer.
        const TRANSFER = 1 << 5;
    }
}

impl Bell {
    /// The events that ring the bell unless `--no-bell` is given.
    pub const DEFAULT: Self = Self::ROOM
        .union(Self::MESSAGE)
        .union(Self::INVITE)
        .union(Self::TRANSFER);

    /// The event of a push with `opcode`.
    pub const fn for_push(opcode: Opcode) -> Self {
        match opcode {
            Opcode::ServerMsg | Opcode::UserBroadcast => Self::MESSAGE,
            Opcode::InviteToChat => Self::INVITE,
            _ => Self::empty(),
        }
    }

    /// The event of a chat line or a news post in `tab`. The event is empty
    /// when `tab` is the `shown` tab and the terminal has the focus.
    pub fn for_tab(tab: Tab, shown: Tab, focused: bool) -> Self {
        if focused && tab == shown {
            return Self::empty();
        }
        match tab {
            Tab::Chat => Self::CHAT,
            Tab::Room(_) => Self::ROOM,
            Tab::News => Self::NEWS,
            _ => Self::empty(),
        }
    }

    /// Ring the bell if one of `events` is in `self`.
    pub fn ring(self, events: Self) -> io::Result<()> {
        if self.intersects(events) {
            let mut out = io::stdout();
            out.write_all(b"\x07")?;
            out.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_push_names_the_message_and_invite_pushes() {
        assert_eq!(Bell::for_push(Opcode::ServerMsg), Bell::MESSAGE);
        assert_eq!(Bell::for_push(Opcode::UserBroadcast), Bell::MESSAGE);
        assert_eq!(Bell::for_push(Opcode::InviteToChat), Bell::INVITE);
        assert_eq!(Bell::for_push(Opcode::ChatMsg), Bell::empty());
    }

    #[test]
    fn for_tab_is_empty_only_for_the_shown_tab_with_the_focus() {
        assert_eq!(Bell::for_tab(Tab::Room(7), Tab::Chat, true), Bell::ROOM);
        assert_eq!(
            Bell::for_tab(Tab::Room(7), Tab::Room(7), true),
            Bell::empty()
        );
        assert_eq!(Bell::for_tab(Tab::Room(7), Tab::Room(7), false), Bell::ROOM);
        assert_eq!(Bell::for_tab(Tab::News, Tab::Chat, true), Bell::NEWS);
        assert_eq!(Bell::for_tab(Tab::Files, Tab::Chat, false), Bell::empty());
    }
}
