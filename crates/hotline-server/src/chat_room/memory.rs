use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;

use async_trait::async_trait;

use crate::chat_room::{ChatRoom, ChatRoomProvider};
use crate::sync::{read, write};

/// A [`ChatRoomProvider`] that keeps the chat rooms in memory.
#[derive(Debug)]
pub struct InMemoryChatRoomProvider {
    rooms: RwLock<BTreeMap<u32, ChatRoom>>,
    next_chat_id: AtomicU32,
}

impl Default for InMemoryChatRoomProvider {
    fn default() -> Self {
        Self {
            rooms: RwLock::new(BTreeMap::from([(0, ChatRoom::default())])),
            next_chat_id: AtomicU32::new(1),
        }
    }
}

#[async_trait]
impl ChatRoomProvider for InMemoryChatRoomProvider {
    async fn create(&self, room: ChatRoom) -> u32 {
        let chat_id = self.next_chat_id.fetch_add(1, Ordering::Relaxed);
        write(&self.rooms).insert(chat_id, room);
        chat_id
    }

    async fn room(&self, chat_id: u32) -> Option<ChatRoom> {
        read(&self.rooms).get(&chat_id).cloned()
    }

    async fn update(
        &self,
        chat_id: u32,
        change: &mut (dyn for<'r> FnMut(&'r mut ChatRoom) -> bool + Send),
    ) -> bool {
        let mut rooms = write(&self.rooms);
        let Some(room) = rooms.get_mut(&chat_id) else {
            return false;
        };
        if !change(room) {
            rooms.remove(&chat_id);
        }
        true
    }

    async fn update_each(
        &self,
        change: &mut (dyn for<'r> FnMut(u32, &'r mut ChatRoom) -> bool + Send),
    ) {
        write(&self.rooms).retain(|&chat_id, room| change(chat_id, room));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(members: Vec<u16>) -> ChatRoom {
        ChatRoom {
            members,
            ..ChatRoom::default()
        }
    }

    #[tokio::test]
    async fn a_new_provider_holds_the_empty_main_room() {
        let provider = InMemoryChatRoomProvider::default();
        assert!(provider.room(0).await.unwrap().members.is_empty());
        assert!(provider.room(1).await.is_none());
    }

    #[tokio::test]
    async fn create_gives_each_room_a_new_chat_id() {
        let provider = InMemoryChatRoomProvider::default();
        let first = provider.create(room(vec![1])).await;
        let second = provider.create(room(vec![2])).await;
        assert_eq!((first, second), (1, 2));
        assert_eq!(provider.room(second).await.unwrap().members, [2]);
    }

    #[tokio::test]
    async fn update_changes_the_room_or_deletes_it() {
        let provider = InMemoryChatRoomProvider::default();
        let chat_id = provider.create(room(vec![1])).await;

        let changed = provider
            .update(chat_id, &mut |room| {
                room.subject = "news".into();
                true
            })
            .await;
        assert!(changed);
        assert_eq!(&*provider.room(chat_id).await.unwrap().subject, "news");

        assert!(provider.update(chat_id, &mut |_| false).await);
        assert!(provider.room(chat_id).await.is_none());
        assert!(!provider.update(chat_id, &mut |_| true).await);
    }

    #[tokio::test]
    async fn update_each_visits_each_room_and_keeps_the_rest() {
        let provider = InMemoryChatRoomProvider::default();
        let chat_id = provider.create(room(vec![1])).await;
        let mut seen = Vec::new();
        provider
            .update_each(&mut |id, _| {
                seen.push(id);
                id != chat_id
            })
            .await;
        assert_eq!(seen, [0, chat_id]);
        assert!(provider.room(0).await.is_some());
        assert!(provider.room(chat_id).await.is_none());
    }
}
