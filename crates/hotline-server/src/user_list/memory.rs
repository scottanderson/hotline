use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::Instant;

use async_trait::async_trait;

use crate::state::UserEntry;
use crate::sync::{read, write};
use crate::user_list::UserListProvider;

/// A [`UserListProvider`] that keeps the users in memory.
#[derive(Debug, Default)]
pub struct InMemoryUserListProvider {
    users: RwLock<BTreeMap<u16, UserEntry>>,
}

#[async_trait]
impl UserListProvider for InMemoryUserListProvider {
    async fn insert(&self, entry: UserEntry) {
        write(&self.users).insert(entry.user_id, entry);
    }

    async fn remove(&self, user_id: u16) {
        write(&self.users).remove(&user_id);
    }

    async fn user(&self, user_id: u16) -> Option<UserEntry> {
        read(&self.users).get(&user_id).cloned()
    }

    async fn users(&self) -> Vec<UserEntry> {
        read(&self.users).values().cloned().collect()
    }

    async fn touch(&self, user_id: u16) {
        if let Some(entry) = write(&self.users).get_mut(&user_id) {
            entry.last_active = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hotline_proto::consts::Access;

    use super::*;
    use crate::account::AccessExtra;

    fn entry(user_id: u16, name: &str) -> UserEntry {
        UserEntry {
            user_id,
            login: Box::default(),
            account_name: Box::default(),
            name: name.into(),
            icon: 0,
            client_version: None,
            access: Access::empty(),
            access_extra: AccessExtra::default(),
            remote_addr: "127.0.0.1:0".parse().unwrap(),
            connected_at: Instant::now(),
            last_active: Instant::now(),
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
        }
    }

    #[tokio::test]
    async fn users_come_in_user_id_order() {
        let provider = InMemoryUserListProvider::default();
        provider.insert(entry(3, "c")).await;
        provider.insert(entry(1, "a")).await;
        provider.insert(entry(2, "b")).await;
        let ids: Vec<u16> = provider.users().await.iter().map(|u| u.user_id).collect();
        assert_eq!(ids, [1, 2, 3]);
    }

    #[tokio::test]
    async fn insert_replaces_and_remove_deletes() {
        let provider = InMemoryUserListProvider::default();
        provider.insert(entry(1, "old")).await;
        provider.insert(entry(1, "new")).await;
        assert_eq!(provider.user(1).await.unwrap().name.as_ref(), "new");
        assert_eq!(provider.users().await.len(), 1);

        provider.remove(1).await;
        assert!(provider.user(1).await.is_none());
    }

    #[tokio::test]
    async fn touch_resets_the_idle_time() {
        let provider = InMemoryUserListProvider::default();
        let mut idle = entry(1, "a");
        idle.last_active -= Duration::from_secs(3600);
        provider.insert(idle).await;
        assert!(provider.user(1).await.unwrap().is_idle());

        provider.touch(1).await;
        assert!(!provider.user(1).await.unwrap().is_idle());
        provider.touch(2).await;
        assert!(provider.user(2).await.is_none());
    }
}
