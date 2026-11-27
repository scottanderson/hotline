use std::borrow::Cow;
use std::net::IpAddr;

use async_trait::async_trait;

use hotline_proto::consts::Access;
use hotline_proto::types::UserFlags;

use crate::account::AccessExtra;
use crate::console::{ChatCommandActor, ChatCommandDispatcher, ConsoleReply, strtoul};
use crate::state::{SharedState, UserEntry};

/// `/users`, `/access`, `/0wn`, and `/unban`. See the module docs for
/// what's deliberately not ported.
#[derive(Debug, Default)]
pub struct DefaultChatCommands;

#[async_trait]
impl ChatCommandDispatcher for DefaultChatCommands {
    async fn run(
        &self,
        command: &str,
        args: &str,
        actor: &ChatCommandActor,
        state: &SharedState,
    ) -> ConsoleReply {
        let reply = match command {
            "users" => cmd_users(actor, state).await,
            "access" => cmd_access(args, actor, state).await,
            "0wn" => cmd_0wn(args, actor, state).await,
            "unban" => cmd_unban(args, actor, state).await,
            _ => return ConsoleReply::Chat(format!("command not found: {command}")),
        };
        reply.unwrap_or_else(|reason| ConsoleReply::Chat(format!("{command}: {reason}")))
    }
}

/// The user whose name starts with `nick`'s first word. An exact match
/// wins over the first prefix match.
fn nick_to_uid(users: &[UserEntry], nick: &str) -> Option<u16> {
    let word = nick.split(' ').next().unwrap_or_default();
    let mut matches = users.iter().filter(|user| user.name.starts_with(word));
    let first = matches.next()?;
    let exact = matches.rfind(|user| user.name.len() == word.len());
    Some(exact.unwrap_or(first).user_id)
}

/// Splits the target off `args`: a user id, or a nickname when the id
/// parses as `0` and `args` doesn't start with `"0 "`.
async fn target<'a>(args: &'a str, state: &SharedState) -> Result<(u32, &'a str), String> {
    let (word, rest) = args.split_once(' ').unwrap_or((args, ""));
    let mut uid = strtoul(args) as u32;
    if uid == 0 && !args.starts_with("0 ") {
        uid = nick_to_uid(&state.users().await, args)
            .ok_or_else(|| format!("no such user \"{word}\""))?
            .into();
    }
    Ok((uid, rest.trim_start_matches(' ')))
}

/// Logs a privilege or appearance change made by a console command.
fn log_console_change(
    command: &str,
    actor: &ChatCommandActor,
    target: &str,
    uid: u32,
    fields: &str,
) {
    tracing::debug!(
        command,
        actor = %actor.login,
        actor_uid = actor.user_id,
        target,
        target_uid = uid,
        fields,
        "console change"
    );
}

/// One `/users` section: a table of `users`, or `none.`.
fn user_table(users: &[UserEntry]) -> String {
    if users.is_empty() {
        return "none.\r".to_string();
    }
    let rows: String = users
        .iter()
        .map(|user| {
            let role = match user.flags().contains(UserFlags::ADMIN) {
                true => "OWNER",
                false => "LUSER",
            };
            format!("{:<3} | {:<25.25} | {role:>4}\r", user.user_id, user.name)
        })
        .collect();
    format!(
        "uid | nickname                  |  priv  \r{}\r{rows}",
        "_".repeat(41)
    )
}

/// `/users`: a private message listing connected users.
async fn cmd_users(
    actor: &ChatCommandActor,
    state: &SharedState,
) -> Result<ConsoleReply, Cow<'static, str>> {
    if !actor.access_extra.contains(AccessExtra::MANAGE_USERS) {
        Err("You are not allowed to manage other users.")?;
    }
    let users = state.users().await;
    Ok(ConsoleReply::Message(format!(
        "(chat 0x0): {} users:\r*** Visible Users ***\r\r{}\r*** Invisible Users ***\r\r{}",
        users.len(),
        user_table(&users),
        // No user is ever invisible here.
        user_table(&[]),
    )))
}

/// `/access <uid|nick>` shows a user's privileges, and `/access <uid|nick>
/// FIELD=0|1 ...` changes them live. Needs [`AccessExtra::USER_ACCESS`].
/// The target must hold [`AccessExtra::ACCESS_VOLATILE`].
async fn cmd_access(
    args: &str,
    actor: &ChatCommandActor,
    state: &SharedState,
) -> Result<ConsoleReply, Cow<'static, str>> {
    if args.is_empty() {
        return Ok(ConsoleReply::Silent);
    }
    if !actor.access_extra.contains(AccessExtra::USER_ACCESS) {
        Err("You are not allowed to view another user's access.")?;
    }
    let (uid, rest) = target(args, state).await?;

    if rest.is_empty() {
        let description = state.describe_access(uid).await?;
        return Ok(ConsoleReply::Chat(description));
    }

    let name = state.apply_access_fields(uid, rest).await?;
    log_console_change("access", actor, &name, uid, rest);
    Ok(ConsoleReply::Silent)
}

/// `/0wn <uid|nick> icon=<n> name=<text>` sets a user's icon or name
/// live. Needs [`AccessExtra::USER_0WN`]. The target must hold
/// [`AccessExtra::IS_0WN3D`]. `name=` takes the rest of the line.
async fn cmd_0wn(
    args: &str,
    actor: &ChatCommandActor,
    state: &SharedState,
) -> Result<ConsoleReply, Cow<'static, str>> {
    if args.is_empty() {
        return Ok(ConsoleReply::Silent);
    }
    if !actor.access_extra.contains(AccessExtra::USER_0WN) {
        Err("You are not allowed to overwrite another user's icon or name.")?;
    }
    let (uid, rest) = target(args, state).await?;
    let name = state.apply_0wn_fields(uid, rest).await?;
    if let Some(name) = name {
        log_console_change("0wn", actor, &name, uid, rest);
    }
    Ok(ConsoleReply::Silent)
}

/// `/unban <address>` lifts an address ban. Needs [`Access::DISCON_USER`],
/// the privilege that bans through [`myTran_DisconnectUser`] (110).
///
/// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
async fn cmd_unban(
    args: &str,
    actor: &ChatCommandActor,
    state: &SharedState,
) -> Result<ConsoleReply, Cow<'static, str>> {
    if args.is_empty() {
        return Ok(ConsoleReply::Silent);
    }
    if !actor.access.contains(Access::DISCON_USER) {
        Err("You are not allowed to disconnect users.")?;
    }
    let word = args.split(' ').next().unwrap_or_default();
    let ip = word
        .parse::<IpAddr>()
        .map_err(|_| format!("not an address \"{word}\""))?;
    state.unban(ip).await?;
    tracing::debug!(actor = %actor.login, actor_uid = actor.user_id, %ip, "console unban");
    Ok(ConsoleReply::Silent)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;

    fn actor(access_extra: AccessExtra) -> ChatCommandActor {
        ChatCommandActor {
            user_id: 1,
            login: "admin".into(),
            access: Access::all(),
            access_extra,
            prefix: '/',
        }
    }

    fn user_entry(user_id: u16, login: &str, name: &str, access_extra: AccessExtra) -> UserEntry {
        UserEntry {
            user_id,
            login: login.into(),
            account_name: name.into(),
            name: name.into(),
            icon: 0,
            client_version: None,
            access: Access::default(),
            access_extra,
            remote_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            connected_at: Instant::now(),
            last_active: Instant::now(),
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
        }
    }

    async fn connect(state: &SharedState, user_id: u16, login: &str, name: &str) {
        state
            .insert_user(user_entry(user_id, login, name, AccessExtra::default()))
            .await;
        state.register_recipient(user_id).await;
    }

    fn chat(reply: ConsoleReply) -> String {
        match reply {
            ConsoleReply::Chat(text) => text,
            other => panic!("expected a chat line, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_matches_a_command_only_in_lowercase() {
        let state = Arc::<SharedState>::default();
        let actor = actor(AccessExtra::MANAGE_USERS);
        let reply = DefaultChatCommands.run("USERS", "", &actor, &state).await;
        assert_eq!(chat(reply), "command not found: USERS");
    }

    #[tokio::test]
    async fn run_matches_only_the_whole_command() {
        let state = Arc::<SharedState>::default();
        let actor = actor(AccessExtra::MANAGE_USERS);
        let reply = DefaultChatCommands
            .run("usersfoo", "", &actor, &state)
            .await;
        assert_eq!(chat(reply), "command not found: usersfoo");
    }

    #[tokio::test]
    async fn run_sends_an_error_after_the_command_name() {
        let state = Arc::<SharedState>::default();
        let actor = actor(AccessExtra::USER_ACCESS);
        let reply = DefaultChatCommands
            .run("access", "99", &actor, &state)
            .await;
        assert_eq!(chat(reply), "access: no such user (uid:99)");
    }

    #[tokio::test]
    async fn users_denied_without_manage_users() {
        let state = Arc::<SharedState>::default();
        let reply = cmd_users(&actor(AccessExtra::empty()), &state)
            .await
            .unwrap_err();
        assert_eq!(reply, "You are not allowed to manage other users.");
    }

    /// The `/users` layout, sent as a private message.
    #[tokio::test]
    async fn users_lists_connected_users_like_mhxd() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;

        let reply = cmd_users(&actor(AccessExtra::MANAGE_USERS), &state).await;

        let rule = "_".repeat(41);
        let expected = format!(
            "(chat 0x0): 1 users:\r*** Visible Users ***\r\r\
             uid | nickname                  |  priv  \r{rule}\r\
             2   | Bob                       | LUSER\r\
             \r*** Invisible Users ***\r\rnone.\r"
        );
        assert_eq!(reply, Ok(ConsoleReply::Message(expected)));
    }

    #[tokio::test]
    async fn empty_arguments_reply_nothing() {
        let state = Arc::<SharedState>::default();
        let actor = actor(AccessExtra::all());
        assert_eq!(
            cmd_access("", &actor, &state).await,
            Ok(ConsoleReply::Silent)
        );
        assert_eq!(cmd_0wn("", &actor, &state).await, Ok(ConsoleReply::Silent));
    }

    #[tokio::test]
    async fn access_denied_without_user_access() {
        let state = Arc::<SharedState>::default();
        let reply = cmd_access("2", &actor(AccessExtra::empty()), &state)
            .await
            .unwrap_err();
        assert_eq!(reply, "You are not allowed to view another user's access.");
    }

    #[tokio::test]
    async fn access_view_reports_current_flags() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;
        let reply = chat(
            cmd_access("2", &actor(AccessExtra::USER_ACCESS), &state)
                .await
                .unwrap(),
        );
        assert!(reply.contains("Bob"));
        assert!(reply.contains("Access:"));
        assert!(reply.contains("AccessExtra:"));
    }

    /// The view has to name flags the way the modify path takes them.
    #[tokio::test]
    async fn access_view_shows_keys_the_modify_path_accepts() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;

        let reply = chat(
            cmd_access("2", &actor(AccessExtra::USER_ACCESS), &state)
                .await
                .unwrap(),
        );

        assert!(reply.contains("download_files"), "{reply}");
        assert!(reply.contains("chat_private"), "{reply}");
        assert!(!reply.contains("DOWNLOAD_FILE"), "{reply}");
    }

    #[tokio::test]
    async fn access_unknown_user_is_reported() {
        let state = Arc::<SharedState>::default();
        let reply = cmd_access("99", &actor(AccessExtra::USER_ACCESS), &state).await;
        assert_eq!(reply.unwrap_err(), "no such user (uid:99)");
    }

    #[tokio::test]
    async fn access_unknown_nickname_is_reported() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;
        let reply = cmd_access("Zed x=1", &actor(AccessExtra::USER_ACCESS), &state).await;
        assert_eq!(reply.unwrap_err(), "no such user \"Zed\"");
    }

    #[tokio::test]
    async fn access_unknown_field_is_rejected() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;
        let reply = cmd_access("2 not_a_field=1", &actor(AccessExtra::USER_ACCESS), &state).await;
        assert_eq!(reply.unwrap_err(), "unknown argument \"not_a_field\"");
    }

    /// A change replies nothing.
    #[tokio::test]
    async fn access_change_by_nickname_replies_nothing() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;
        let reply = cmd_access(
            "Bob delete_files=1",
            &actor(AccessExtra::USER_ACCESS),
            &state,
        )
        .await;
        assert_eq!(reply, Ok(ConsoleReply::Silent));
    }

    #[tokio::test]
    async fn access_modify_never_persists_to_the_account() {
        let state = Arc::<SharedState>::default();
        state
            .put_account(
                "bob",
                crate::account::Account {
                    password: Box::default(),
                    name: "Bob".into(),
                    access: Access::default(),
                    access_extra: AccessExtra::default(),
                },
            )
            .await
            .unwrap();
        connect(&state, 2, "bob", "Bob").await;

        let reply = cmd_access("2 delete_files=1", &actor(AccessExtra::USER_ACCESS), &state).await;
        assert_eq!(reply, Ok(ConsoleReply::Silent));

        let account = state.account("bob").await.expect("account exists");
        assert!(!account.access.contains(Access::DELETE_FILE));
    }

    #[tokio::test]
    async fn zeroown_denied_without_user_0wn() {
        let state = Arc::<SharedState>::default();
        let reply = cmd_0wn("2 icon=5", &actor(AccessExtra::empty()), &state)
            .await
            .unwrap_err();
        assert_eq!(
            reply,
            "You are not allowed to overwrite another user's icon or name."
        );
    }

    #[tokio::test]
    async fn zeroown_change_replies_nothing() {
        let state = Arc::<SharedState>::default();
        connect(&state, 2, "bob", "Bob").await;
        let reply = cmd_0wn("2 icon=5", &actor(AccessExtra::USER_0WN), &state).await;
        assert_eq!(reply, Ok(ConsoleReply::Silent));
    }

    #[test]
    fn nick_to_uid_prefers_an_exact_match() {
        let users = [
            user_entry(2, "a", "Bobby", AccessExtra::default()),
            user_entry(3, "b", "Bob", AccessExtra::default()),
        ];
        assert_eq!(nick_to_uid(&users, "Bob rest"), Some(3));
        assert_eq!(nick_to_uid(&users, "Bo"), Some(2));
        assert_eq!(nick_to_uid(&users, "Al"), None);
    }

    const BANNED: &str = "10.0.0.1";

    #[tokio::test]
    async fn unban_denied_without_discon_user() {
        let state = Arc::<SharedState>::default();
        let actor = ChatCommandActor {
            access: Access::empty(),
            ..actor(AccessExtra::all())
        };
        let reply = cmd_unban(BANNED, &actor, &state).await.unwrap_err();
        assert_eq!(reply, "You are not allowed to disconnect users.");
    }

    #[tokio::test]
    async fn unban_lifts_a_ban() {
        let state = Arc::<SharedState>::default();
        let ip: IpAddr = BANNED.parse().unwrap();
        state.ban(crate::BanRecord::new(ip, None)).await.unwrap();

        let reply = cmd_unban(BANNED, &actor(AccessExtra::empty()), &state).await;

        assert_eq!(reply, Ok(ConsoleReply::Silent));
        assert!(state.is_banned(ip).await.is_none());
    }

    #[tokio::test]
    async fn unban_reports_an_address_that_is_not_banned() {
        let state = Arc::<SharedState>::default();
        let reply = cmd_unban(BANNED, &actor(AccessExtra::empty()), &state).await;
        assert_eq!(reply.unwrap_err(), "address is not banned");
    }

    #[tokio::test]
    async fn unban_rejects_a_word_that_is_not_an_address() {
        let state = Arc::<SharedState>::default();
        let reply = cmd_unban("bob", &actor(AccessExtra::empty()), &state).await;
        assert_eq!(reply.unwrap_err(), "not an address \"bob\"");
    }
}
