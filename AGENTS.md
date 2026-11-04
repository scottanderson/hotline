# hotline-rs

A Rust client, server, and tracker for the Hotline protocols.

- `README.md`: the crates, their status, and how to build them
- `crates/*/README.md`: how to use each crate, and its design. The
  `hotline-daemon` README tells how to run `hls`.
- `plan.md`: open work and known differences from openline
- `HLProtocol.md`: the protocol reference

Put details in those files. Keep this file to rules.

## Writing

Write in ASD-STE100 (Simplified Technical English). Use short
sentences, one idea in each sentence, common words, and the active
voice. This applies to replies, comments, docs, and commit messages.

## Behavior

- openline is the source of truth. Use mhxd to cross-check when
  openline is unclear or silent.
- Name openline or mhxd in each citation in commits, `plan.md`, and
  conversation.
- In a code comment, state the behavior. Name a source only for a
  deliberate divergence.
- Port openline's behavior, not its code. Write idiomatic Rust when
  the result is the same for each input. Keep openline's form only
  where it changes the behavior, such as the order of side effects or
  a boundary.
- Ask when a behavior is unclear.

## Naming

- An `Opcode` or `FieldId` variant is the exact tail of its
  `HLProtocol.md` constant: `myField_UserID` is `UserID`. If
  `HLProtocol.md` has no name, use openline's
  `HotlineClientServerCommon.h`, then mhxd's `HTLC_*`/`HTLS_*`.
- A server handler is `on_` plus the snake_case variant. An `hlc` push
  handler is `apply_` plus the snake_case variant. A handler's tests
  start with the snake_case variant, without `on_` or `apply_`. A test
  of more than one handler starts with the first handler.
- Cite an opcode or field with its decimal value: `myTran_Login` (107).
  In `///` and `//!`, make it a reference link:
  ``[`myTran_Login`]: hotline_proto::Opcode::Login``. Use `crate::`
  inside `hotline-proto`.

## Code

- `hotline-proto` has no I/O and no async.
- Every text conversion uses the connection's `&'static Encoding`.
- `Session` holds per-connection state. `SharedState` holds shared
  state. Do not keep a copy of a fact in both.
- Sessions reach each other only through the `SharedState` channel for
  each recipient. The receiving session filters by privilege
  (`Session::may_receive`).
- The server codec numbers every push. A reply echoes the `trans` of
  its request.
- Put each handler in the `session/` file for its area.
- Check a privilege at the top of its handler, not in the
  `Session::dispatch` `select!` loop.
- Make a new privilege bit part of a `bitflags!` type, not a lone
  `bool`.
- Prefer named concrete helpers to one generic helper with a trait
  parameter.
- Prefer a method chain, and pass an associated function directly, as
  in `.map(Field::encode)`, when the chain stays easy to read.
- A handler gets its validation from a `SharedState` call. The call
  returns `Result<T, &'static str>`, or `Result<T, Cow<'static, str>>`
  when the error has formatted text. The handler replies with
  `tx.reply_error(reason)`.
- Make a `Cow<'static, str>` error with `?` from a `&'static str` or a
  `String`. Do not write `Cow::Borrowed` or `Cow::Owned`.
- Privilege errors come from `RequireFlags::require`, which returns a
  `String`, and from the `actions()` tables. The one exception is the
  target checks for `/access` and `/0wn` in `console/`. They use mhxd's
  message.
- A fixed-size wire struct has a `pub const LEN`. Read it with
  `PacketReader`. Write it with `PacketWriter`, or `VecWriter` for a
  variable size.
- Convert opcodes with `Opcode::from(v)` and `u32::from(opcode)`.
- Let the compiler infer a const generic when its use fixes the
  value, as in `HlDate::decode(r.take())`. Name it only when nothing
  else does, as in a `take` that skips bytes.
- In a trait impl, write associated types as `Self::Item`.
- Derive `serde` on the domain type. Give one field a custom format
  with a `#[serde(with = "module")]` module, like `news/iso_date.rs`.
- Write a `serde` impl by hand only when a field's format depends on
  another field, or when the stored shape differs from the type, as in
  `news/category.rs`. Derive the other direction when you can. Do not
  add a struct that mirrors the whole type.

## Comments

- Write a `//` comment on one line. A `///` or `//!` comment can be
  longer, but keep it short.
- Start a doc comment with a one-line summary. Add a `# Panics`
  section when the item can panic. Use markdown and no stand-alone
  dashes.
- State each fact once. Leave out "since X" and "not Y" clauses, and
  reasons for a signature's shape. Put the reasons in the commit
  message.
- Change a comment only with the code it describes.

## Logging

Use `tracing`. Use `error!` and `warn!` for problems. Use `debug!` for
routine events on one connection or transfer. Use `info!` for rare
server events, such as startup. A path that a client can repeat logs
at `debug!`.

## Checks

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
cargo test --workspace
cargo test -p hotline-server <test_name>
gitlint --commits main..HEAD
```

Session tests build their fixtures with `test_session` in
`crates/hotline-server/src/session/test_support.rs`, without a socket.

## Git

- Make small commits that each build and pass the tests. Before you
  split a change, show the planned commits to the user.
- Write the subject as `area: summary`. The summary is a short
  imperative sentence that starts with a lowercase letter. The area
  is the crate that the change is about, without `hotline-`, such as
  `proto`, `server`, `client`, or `daemon`. Add a module path when
  the change stays in one module, such as `server/session`. Use
  `htxf` for a file transfer change that is about more than one
  crate, `plan` for a change only to `plan.md`, `agents` for a change
  only to `AGENTS.md`, `gitlint` for `.gitlint`, and `workflows` for
  `.github/workflows/`.
- Put the reasons in the body. Add `Fixes #N` when a commit closes an
  issue. Put issue references only in commit messages.
- Stage files by name. Keep the changes the user already staged.
- Put a `plan.md` update in the commit for its work.
- End with one `Co-Authored-By:` trailer for each model that worked on
  the change. Name the exact model, for example
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Start the next backlog item only when the user says so.
