use std::collections::HashSet;

/// A chat room: its member user IDs, in join order, plus a subject
/// line.
#[derive(Debug, Clone, Default)]
pub struct ChatRoom {
    /// Currently joined members, in the order they joined.
    pub members: Vec<u16>,
    /// Room subject, settable by any member.
    pub subject: Box<str>,
    /// Users invited but not yet joined.
    pub invited: HashSet<u16>,
    /// Join password. Empty means the room is unprotected.
    pub password: Box<str>,
}

impl ChatRoom {
    /// Add `user_id` with its pending invite, or with the room's password
    /// when the room has one.
    pub(crate) fn join(&mut self, user_id: u16, password: &str) -> Result<(), &'static str> {
        let invited = self.invited.remove(&user_id);
        if !invited {
            if self.password.is_empty() {
                return Err("not invited");
            }
            if password != &*self.password {
                return Err("wrong password");
            }
        }
        self.add_member(user_id);
        Ok(())
    }

    /// Add `user_id` with no invite or password check, and clear its
    /// invite.
    pub(crate) fn add_member(&mut self, user_id: u16) {
        if !self.members.contains(&user_id) {
            self.members.push(user_id);
        }
        self.invited.remove(&user_id);
    }

    /// Invite `target_id` on behalf of the member `inviter`.
    pub(crate) fn invite_by(&mut self, inviter: u16, target_id: u16) -> Result<(), &'static str> {
        if !self.members.contains(&inviter) {
            return Err("you are not in that chat room");
        }
        if self.members.contains(&target_id) {
            return Err("that user is already in the chat");
        }
        if !self.invited.insert(target_id) {
            return Err("that user is already invited to the chat");
        }
        Ok(())
    }

    /// Remove the member `user_id`, and return the members that remain.
    /// `None` if `user_id` is not a member.
    pub(crate) fn leave(&mut self, user_id: u16) -> Option<Vec<u16>> {
        let position = self.members.iter().position(|&id| id == user_id)?;
        self.members.remove(position);
        Some(self.members.clone())
    }
}
