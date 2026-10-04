//! Private chat rooms.

use hotline_client::{HotlineClient, JoinedChat};
use hotline_proto::{Field, FieldId, Transaction};

use super::{display_text, wire_text, ChatInvite, ChatRoom, ClientSession};
use crate::line::LineKind;

/// The most invitations that wait for an answer.
const MAX_INVITES: usize = 16;

impl ClientSession {
    /// The room `chat_id`, if this client is in it.
    pub fn room(&self, chat_id: u32) -> Option<&ChatRoom> {
        self.rooms.iter().find(|room| room.id == chat_id)
    }

    fn room_mut(&mut self, chat_id: u32) -> Option<&mut ChatRoom> {
        self.rooms.iter_mut().find(|room| room.id == chat_id)
    }

    /// A push's [`myField_ChatID`] (114), if it names a room this client is in.
    ///
    /// [`myField_ChatID`]: hotline_proto::FieldId::ChatID
    fn push_room(&mut self, tx: &Transaction) -> Option<&mut ChatRoom> {
        let chat_id = tx.field(FieldId::ChatID).and_then(Field::as_u32)?;
        self.room_mut(chat_id)
    }

    /// [`myTran_ChatMsg`] (106) with a [`myField_ChatID`] (114): one room's chat.
    /// `false` if the push names no room this client is in.
    ///
    /// [`myTran_ChatMsg`]: hotline_proto::Opcode::ChatMsg
    /// [`myField_ChatID`]: hotline_proto::FieldId::ChatID
    pub(super) fn room_chat_msg(&mut self, tx: &Transaction) -> bool {
        let encoding = self.encoding.get();
        let text = tx.string(FieldId::Data, encoding).unwrap_or_default();
        let Some(room) = self.push_room(tx) else {
            return false;
        };
        for line in display_text(&text).split('\r') {
            room.push(line, LineKind::Chat);
        }
        true
    }

    /// [`myTran_InviteToChat`] (113): an invitation to a room. Ignores a
    /// second invitation to a waiting room, and any past [`MAX_INVITES`].
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    pub(super) fn apply_invite_to_chat(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let Some(chat_id) = tx.field(FieldId::ChatID).and_then(Field::as_u32) else {
            return;
        };
        if self.invites.len() >= MAX_INVITES
            || self.invites.iter().any(|invite| invite.chat_id == chat_id)
        {
            return;
        }
        let from = tx
            .string(FieldId::UserName, encoding)
            .unwrap_or_else(|| "someone".to_string());
        self.push(
            format!("{from} invited you to a private chat"),
            LineKind::System,
        );
        self.invites.push_back(ChatInvite {
            chat_id,
            from: from.into(),
        });
    }

    /// [`myTran_NotifyChatChangeUser`] (117): a user joined a room or changed.
    ///
    /// [`myTran_NotifyChatChangeUser`]: hotline_proto::Opcode::NotifyChatChangeUser
    pub(super) fn apply_notify_chat_change_user(&mut self, tx: &Transaction) {
        let Some(user) = tx.user_entry(self.encoding.get()) else {
            return;
        };
        let Some(room) = self.push_room(tx) else {
            return;
        };
        match room.users.iter_mut().find(|u| u.user_id == user.user_id) {
            Some(known) => *known = user,
            None => {
                room.push(format!("{} joined", user.name), LineKind::System);
                room.users.push(user);
            }
        }
    }

    /// [`myTran_NotifyChatDeleteUser`] (118): a user left a room.
    ///
    /// [`myTran_NotifyChatDeleteUser`]: hotline_proto::Opcode::NotifyChatDeleteUser
    pub(super) fn apply_notify_chat_delete_user(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(FieldId::UserID).and_then(Field::as_u16) else {
            return;
        };
        let Some(room) = self.push_room(tx) else {
            return;
        };
        if let Some(pos) = room.users.iter().position(|u| u.user_id == user_id) {
            let user = room.users.remove(pos);
            room.push(format!("{} left", user.name), LineKind::System);
        }
    }

    /// [`myTran_NotifyChatSubject`] (119): a room's new subject.
    ///
    /// [`myTran_NotifyChatSubject`]: hotline_proto::Opcode::NotifyChatSubject
    pub(super) fn apply_notify_chat_subject(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let Some(subject) = tx.string(FieldId::ChatSubject, encoding) else {
            return;
        };
        match self.push_room(tx) {
            Some(room) => {
                room.push(format!("subject: {subject}"), LineKind::System);
                room.subject = subject.into();
            }
            None => self.push(format!("subject: {subject}"), LineKind::System),
        }
    }

    /// Open a new room with `user_id`, [`myTran_InviteNewChat`] (112).
    /// Returns the room's ID.
    ///
    /// [`myTran_InviteNewChat`]: hotline_proto::Opcode::InviteNewChat
    pub async fn invite_new_chat(&mut self, client: &HotlineClient, user_id: u16) -> Option<u32> {
        match client.invite_new_chat(user_id).await {
            Ok(joined) => Some(self.add_room(joined)),
            Err(e) => {
                self.push(format!("failed to open a chat: {e}"), LineKind::Error);
                None
            }
        }
    }

    /// Invite `user_id` to the room `chat_id`, [`myTran_InviteToChat`] (113).
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    pub async fn invite_to_chat(&mut self, client: &HotlineClient, chat_id: u32, user_id: u16) {
        let name = self.user_name(user_id);
        let (text, kind) = match client.invite_to_chat(chat_id, user_id).await {
            Ok(()) => (format!("invited {name}"), LineKind::System),
            Err(e) => (format!("failed to invite {name}: {e}"), LineKind::Error),
        };
        if let Some(room) = self.room_mut(chat_id) {
            room.push(text, kind);
        }
    }

    /// Join the room `chat_id`, [`myTran_JoinChat`] (115). Returns the room's ID.
    ///
    /// [`myTran_JoinChat`]: hotline_proto::Opcode::JoinChat
    pub async fn join_chat(&mut self, client: &HotlineClient, chat_id: u32) -> Option<u32> {
        match client.join_chat(chat_id).await {
            Ok(joined) => Some(self.add_room(joined)),
            Err(e) => {
                self.push(format!("failed to join the chat: {e}"), LineKind::Error);
                None
            }
        }
    }

    fn add_room(&mut self, joined: JoinedChat) -> u32 {
        let id = joined.id;
        self.rooms.retain(|room| room.id != id);
        self.rooms.push(joined.into());
        id
    }

    /// Decline the invitation to the room `chat_id`,
    /// [`myTran_RejectChatInvite`] (114).
    ///
    /// [`myTran_RejectChatInvite`]: hotline_proto::Opcode::RejectChatInvite
    pub async fn reject_chat_invite(&mut self, client: &HotlineClient, chat_id: u32) {
        if let Err(e) = client.reject_chat_invite(chat_id).await {
            self.push(format!("failed to decline: {e}"), LineKind::Error);
        }
    }

    /// Leave the room `chat_id`, [`myTran_LeaveChat`] (116).
    ///
    /// [`myTran_LeaveChat`]: hotline_proto::Opcode::LeaveChat
    pub async fn leave_chat(&mut self, client: &HotlineClient, chat_id: u32) {
        self.rooms.retain(|room| room.id != chat_id);
        if let Err(e) = client.leave_chat(chat_id).await {
            self.push(format!("failed to leave the chat: {e}"), LineKind::Error);
        }
    }

    /// Set the subject of the room `chat_id`, [`myTran_SetChatSubject`] (120).
    ///
    /// [`myTran_SetChatSubject`]: hotline_proto::Opcode::SetChatSubject
    pub async fn set_chat_subject(&mut self, client: &HotlineClient, chat_id: u32, subject: &str) {
        if let Err(e) = client.set_chat_subject(chat_id, subject).await {
            if let Some(room) = self.room_mut(chat_id) {
                room.push(format!("failed to set the subject: {e}"), LineKind::Error);
            }
        }
    }

    /// Send `line` to the room `chat_id`.
    pub async fn send_room_chat(&mut self, client: &HotlineClient, chat_id: u32, line: &str) {
        if let Err(e) = client.send_room_chat(chat_id, &wire_text(line)).await {
            if let Some(room) = self.room_mut(chat_id) {
                room.push(format!("failed to send: {e}"), LineKind::Error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::text_encoding::EncodingHandle;
    use hotline_proto::types::{UserFlags, UserNameWithInfo};
    use hotline_proto::Opcode;

    use super::*;
    use crate::session::test_support;

    fn session_in_room(chat_id: u32) -> ClientSession {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        session.rooms.push(ChatRoom {
            id: chat_id,
            subject: "".into(),
            users: vec![UserNameWithInfo {
                user_id: 1,
                icon: 0,
                flags: UserFlags::empty(),
                name: "me".into(),
            }],
            lines: Vec::new(),
        });
        session
    }

    fn room_push(opcode: Opcode, chat_id: u32, mut fields: Vec<Field>) -> Transaction {
        fields.push(Field::u32(FieldId::ChatID, chat_id));
        Transaction::push(opcode, fields)
    }

    #[test]
    fn invite_to_chat_queues_one_invitation_per_room() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let invite = |chat_id| {
            Transaction::push(
                Opcode::InviteToChat,
                vec![
                    Field::u32(FieldId::ChatID, chat_id),
                    Field::string(FieldId::UserName, "Alice"),
                ],
            )
        };
        let _ = session.apply_server_event(&invite(7));
        let _ = session.apply_server_event(&invite(7));
        assert_eq!(session.invites.len(), 1);

        for chat_id in 0..100 {
            let _ = session.apply_server_event(&invite(chat_id));
        }
        assert_eq!(session.invites.len(), MAX_INVITES);
    }

    #[test]
    fn chat_msg_with_a_chat_id_goes_to_the_room() {
        let mut session = session_in_room(7);
        let before = session.lines.len();
        let msg = room_push(
            Opcode::ChatMsg,
            7,
            vec![Field::string(FieldId::Data, "\rAlice: hi")],
        );
        let _ = session.apply_server_event(&msg);
        assert_eq!(session.lines.len(), before);
        assert_eq!(&*session.rooms[0].lines[0].text, "Alice: hi");
    }

    /// A [`myTran_NotifyChatChangeUser`] (117) that adds Alice to room 7.
    ///
    /// [`myTran_NotifyChatChangeUser`]: hotline_proto::Opcode::NotifyChatChangeUser
    fn alice_joins() -> Transaction {
        room_push(
            Opcode::NotifyChatChangeUser,
            7,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::string(FieldId::UserName, "Alice"),
            ],
        )
    }

    #[test]
    fn notify_chat_change_user_adds_the_member() {
        let mut session = session_in_room(7);
        let _ = session.apply_server_event(&alice_joins());
        assert_eq!(session.rooms[0].users.len(), 2);
        let texts: Vec<_> = session.rooms[0].lines.iter().map(|l| &*l.text).collect();
        assert_eq!(texts, ["Alice joined"]);
    }

    #[test]
    fn notify_chat_delete_user_removes_the_member() {
        let mut session = session_in_room(7);
        let _ = session.apply_server_event(&alice_joins());
        let leave = room_push(
            Opcode::NotifyChatDeleteUser,
            7,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let _ = session.apply_server_event(&leave);
        assert_eq!(session.rooms[0].users.len(), 1);
        let texts: Vec<_> = session.rooms[0].lines.iter().map(|l| &*l.text).collect();
        assert_eq!(texts, ["Alice joined", "Alice left"]);
    }

    #[test]
    fn notify_chat_subject_renames_the_room() {
        let mut session = session_in_room(7);
        assert_eq!(session.rooms[0].label(), "Chat 7");
        let subject = room_push(
            Opcode::NotifyChatSubject,
            7,
            vec![Field::string(FieldId::ChatSubject, "plans")],
        );
        let _ = session.apply_server_event(&subject);
        assert_eq!(session.rooms[0].label(), "plans");
    }

    #[tokio::test]
    async fn join_chat_adds_the_room_and_leave_chat_removes_it() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (alice, _alice_events) = test_support::join_admin(addr, "alice").await;
        let (bob, mut bob_events) = test_support::join_admin(addr, "bob").await;
        let users = alice.get_user_name_list().await.unwrap();
        let bob_id = users.iter().find(|u| &*u.name == "bob").unwrap().user_id;

        let mut alice_session = ClientSession::new("alice".to_string(), 0, alice.encoding());
        let chat_id = alice_session.invite_new_chat(&alice, bob_id).await.unwrap();

        let invite = test_support::next_push(&mut bob_events, Opcode::InviteToChat).await;
        let mut bob_session = ClientSession::new("bob".to_string(), 0, bob.encoding());
        let _ = bob_session.apply_server_event(&invite);
        let invite = bob_session.invites.pop_front().unwrap();
        assert_eq!(invite.chat_id, chat_id);

        bob_session.join_chat(&bob, chat_id).await.unwrap();
        assert_eq!(bob_session.room(chat_id).unwrap().users.len(), 2);

        bob_session.leave_chat(&bob, chat_id).await;
        assert!(bob_session.room(chat_id).is_none());
    }
}
