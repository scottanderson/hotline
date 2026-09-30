//! An [`ActivityLog`] that appends to openline's tab-separated log files.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::Context;
use async_trait::async_trait;
use hotline_server::{AccountAction, Activity, ActivityEntry, ActivityLog};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const FILE_HEADER: &str = "Date/time\tAddress\tAccount\tName\tFile Name\n";
const CONNECT_HEADER: &str = "Date/time\tAddress\tAccount\tName\n";
const ACCOUNT_HEADER: &str = "Date/time\tAddress\tAccount\tName\tAction\tAffected Account\n";

/// Lines the writer task can fall behind by before `record` waits.
const QUEUE_LEN: usize = 1024;

/// One optional log file for each kind of activity. An unset path logs nothing.
#[derive(Debug, Default)]
pub struct LogPaths {
    pub downloads: Option<PathBuf>,
    pub uploads: Option<PathBuf>,
    pub connects: Option<PathBuf>,
    pub accounts: Option<PathBuf>,
}

impl LogPaths {
    /// Check that each set path opens for appending.
    pub async fn check(&self) -> anyhow::Result<()> {
        let flags = [
            (&self.downloads, "--download-log"),
            (&self.uploads, "--upload-log"),
            (&self.connects, "--connect-log"),
            (&self.accounts, "--account-log"),
        ];
        for (path, flag) in flags {
            if let Some(path) = path {
                check_path(path).await.with_context(|| {
                    format!("failed to open log file {} ({flag})", path.display())
                })?;
            }
        }
        Ok(())
    }
}

/// One line for the writer task to append.
#[derive(Debug)]
struct Line {
    path: PathBuf,
    header: &'static str,
    text: String,
}

/// Queues each [`ActivityEntry`] for a writer task that appends it to its
/// file in [`LogPaths`].
#[derive(Debug)]
pub struct FileActivityLog {
    paths: LogPaths,
    queue: mpsc::Sender<Line>,
}

impl FileActivityLog {
    /// Start the writer task. It stops after the log is dropped.
    pub fn new(paths: LogPaths) -> (Self, JoinHandle<()>) {
        let (queue, lines) = mpsc::channel(QUEUE_LEN);
        let writer = tokio::spawn(write_lines(lines));
        (Self { paths, queue }, writer)
    }

    fn target<'a>(
        &self,
        activity: &'a Activity,
    ) -> Option<(&Path, &'static str, Vec<Cow<'a, str>>)> {
        let paths = &self.paths;
        let (path, header, columns) = match activity {
            Activity::Connect => (&paths.connects, CONNECT_HEADER, vec![]),
            Activity::Download { name } => (&paths.downloads, FILE_HEADER, vec![name.into()]),
            Activity::Upload { name } => (&paths.uploads, FILE_HEADER, vec![name.into()]),
            Activity::AccountChange { action, account } => {
                let affected = match action {
                    AccountAction::Rename { from } => format!("“{from}” “{account}”").into(),
                    _ => account.into(),
                };
                let columns = vec![action.label().into(), affected];
                (&paths.accounts, ACCOUNT_HEADER, columns)
            }
        };
        Some((path.as_deref()?, header, columns))
    }
}

/// Check that `path` opens for appending, and create it if it is missing.
async fn check_path(path: &Path) -> std::io::Result<()> {
    open_for_append(path).await.map(drop)
}

async fn open_for_append(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
}

async fn write_lines(mut lines: mpsc::Receiver<Line>) {
    while let Some(line) = lines.recv().await {
        if let Err(error) = append(&line).await {
            tracing::warn!(path = %line.path.display(), %error, "failed to write activity log");
        }
    }
}

async fn append(line: &Line) -> std::io::Result<()> {
    let mut file = open_for_append(&line.path).await?;
    if file.metadata().await?.len() == 0 {
        file.write_all(line.header.as_bytes()).await?;
    }
    file.write_all(line.text.as_bytes()).await?;
    file.flush().await
}

/// Append a tab and `field`, with each control character as a space.
fn push_column(text: &mut String, field: &str) {
    text.push('\t');
    text.extend(field.chars().map(|c| if c.is_control() { ' ' } else { c }));
}

#[async_trait]
impl ActivityLog for FileActivityLog {
    async fn record(&self, entry: ActivityEntry) {
        let Some((path, header, columns)) = self.target(&entry.activity) else {
            return;
        };
        let mut text = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        push_column(&mut text, &entry.address.to_string());
        push_column(&mut text, &entry.login);
        push_column(&mut text, &entry.name);
        for column in &columns {
            push_column(&mut text, column);
        }
        text.push('\n');
        let line = Line {
            path: path.to_path_buf(),
            header,
            text,
        };
        // Fails only if the writer task panicked.
        let _ = self.queue.send(line).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(activity: Activity) -> ActivityEntry {
        ActivityEntry {
            address: "10.0.0.1".parse().unwrap(),
            login: "guest".to_string(),
            name: "Alice".to_string(),
            activity,
        }
    }

    /// Record each entry, then wait until the writer task has written them.
    async fn record_all(paths: LogPaths, entries: impl IntoIterator<Item = ActivityEntry>) {
        let (log, writer) = FileActivityLog::new(paths);
        for entry in entries {
            log.record(entry).await;
        }
        drop(log);
        writer.await.unwrap();
    }

    /// The header, then each line without its date column, which must parse.
    fn lines(path: &Path) -> (String, Vec<String>) {
        let text = std::fs::read_to_string(path).unwrap();
        let mut lines = text.lines();
        let header = lines.next().unwrap().to_string();
        let rest = lines
            .map(|line| {
                let (date, rest) = line.split_once('\t').unwrap();
                chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%d %H:%M:%S").unwrap();
                rest.to_string()
            })
            .collect();
        (header, rest)
    }

    #[tokio::test]
    async fn each_activity_writes_openline_s_header_and_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let paths = LogPaths {
            downloads: Some(path("downloads.log")),
            uploads: Some(path("uploads.log")),
            connects: Some(path("connects.log")),
            accounts: Some(path("accounts.log")),
        };
        let account_change = |action, account: &str| Activity::AccountChange {
            action,
            account: account.to_string(),
        };
        let rename = AccountAction::Rename {
            from: "carol".to_string(),
        };
        let activities = [
            Activity::Connect,
            Activity::Download {
                name: "a.txt".to_string(),
            },
            Activity::Upload {
                name: "b.txt".to_string(),
            },
            account_change(AccountAction::Create, "carol"),
            account_change(AccountAction::Modify, "carol"),
            account_change(rename, "caroline"),
            account_change(AccountAction::Delete, "caroline"),
        ];
        record_all(paths, activities.map(entry)).await;

        let file_header = "Date/time\tAddress\tAccount\tName\tFile Name";
        let expected: [(&str, &str, &[&str]); 4] = [
            (
                "connects.log",
                "Date/time\tAddress\tAccount\tName",
                &["10.0.0.1\tguest\tAlice"],
            ),
            (
                "downloads.log",
                file_header,
                &["10.0.0.1\tguest\tAlice\ta.txt"],
            ),
            (
                "uploads.log",
                file_header,
                &["10.0.0.1\tguest\tAlice\tb.txt"],
            ),
            (
                "accounts.log",
                "Date/time\tAddress\tAccount\tName\tAction\tAffected Account",
                &[
                    "10.0.0.1\tguest\tAlice\tCreate\tcarol",
                    "10.0.0.1\tguest\tAlice\tModify\tcarol",
                    "10.0.0.1\tguest\tAlice\tRename\t“carol” “caroline”",
                    "10.0.0.1\tguest\tAlice\tDelete\tcaroline",
                ],
            ),
        ];
        for (file, header, rest) in expected {
            let (actual_header, actual_rest) = lines(&path(file));
            assert_eq!(actual_header, header, "{file}");
            assert_eq!(actual_rest, rest, "{file}");
        }
    }

    #[tokio::test]
    async fn control_characters_cannot_add_columns_or_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connects.log");
        let paths = LogPaths {
            connects: Some(path.clone()),
            ..LogPaths::default()
        };
        let mut entry = entry(Activity::Connect);
        entry.name = "Al\tice\r\nfake".to_string();
        record_all(paths, [entry]).await;

        let (_, rest) = lines(&path);
        assert_eq!(rest, ["10.0.0.1\tguest\tAl ice  fake"]);
    }

    #[tokio::test]
    async fn an_unset_path_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = LogPaths {
            connects: Some(dir.path().join("connects.log")),
            ..LogPaths::default()
        };
        let name = "a.txt".to_string();
        record_all(paths, [entry(Activity::Upload { name })]).await;
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn check_path_rejects_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("connects.log");
        assert!(check_path(&path).await.is_err());
        assert!(check_path(&dir.path().join("connects.log")).await.is_ok());
    }
}
