//! The requests that run off the event loop, and the rows of the tasks box.

use std::time::{Duration, Instant};

use hotline_client::HotlineClient;
use tokio::task::JoinSet;

use crate::session::{ClientSession, Next, Request};
use crate::view::View;

/// How long the finished tasks stay in the tasks box after the last one
/// ends.
const KEEP_FINISHED: Duration = Duration::from_millis(500);

/// What a reply does to the session and the view.
pub type Step = Box<dyn FnOnce(&mut ClientSession, &mut View, &mut Tasks) + Send>;

/// A task in the tasks box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskId {
    Transfer(u64),
}

/// One row of the tasks box.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: TaskId,
    pub label: String,
    pub cancellable: bool,
}

impl TaskRow {
    /// The row of this task after it ends well.
    fn finished(self) -> Self {
        Self {
            cancellable: false,
            ..self
        }
    }
}

/// The requests that run off the event loop.
pub struct Tasks {
    set: JoinSet<Step>,
    /// The rows of the running tasks that `rows` gave last, with their start.
    shown: Vec<(Instant, TaskRow)>,
    /// The rows of the tasks that ended well, with their start.
    finished: Vec<(Instant, TaskRow)>,
    /// When the last task ended well.
    finished_at: Instant,
    pub client: HotlineClient,
}

impl Tasks {
    pub fn new(client: &HotlineClient) -> Self {
        Self {
            set: JoinSet::new(),
            shown: Vec::new(),
            finished: Vec::new(),
            finished_at: Instant::now(),
            client: client.clone(),
        }
    }

    /// Run `request`, then give its value to `then`.
    pub fn start<T: Send + 'static>(
        &mut self,
        request: Request<T>,
        then: impl FnOnce(T, &mut ClientSession, &mut View, &mut Self) + Send + 'static,
    ) {
        self.set.spawn(async move {
            let apply = request.reply().await;
            let step: Step = Box::new(move |session, view, tasks| match apply(session) {
                Next::Done(value) => then(value, session, view, tasks),
                Next::Then(request) => tasks.start(request, then),
            });
            step
        });
    }

    /// Run `request`.
    pub fn run(&mut self, request: Request<()>) {
        self.start(request, |(), _, _, _| {});
    }

    /// Remove task `id` from the box when it ends, with no finished row.
    pub fn forget(&mut self, id: TaskId) {
        self.shown.retain(|(_, row)| row.id != id);
    }

    /// The step of the next reply. `None` when no request runs.
    pub async fn next(&mut self) -> Option<Step> {
        loop {
            match self.set.join_next().await? {
                Ok(step) => return Some(step),
                Err(e) if e.is_cancelled() => {}
                Err(e) => std::panic::resume_unwind(e.into_panic()),
            }
        }
    }

    /// The rows of the tasks box: each transfer of `session`, and each task
    /// that ended well, in the order the user started them. The finished
    /// rows go [`KEEP_FINISHED`] after the last task ends. A transfer that
    /// failed goes at once.
    pub fn rows(&mut self, session: &mut ClientSession) -> Vec<TaskRow> {
        for id in session.end_transfers() {
            self.forget(id);
        }
        let mut rows: Vec<_> = session.transfer_rows().collect();
        let now = Instant::now();
        let count = self.finished.len();
        let ended = self
            .shown
            .drain(..)
            .filter(|(_, row)| rows.iter().all(|(_, r)| r.id != row.id));
        self.finished
            .extend(ended.map(|(started, row)| (started, row.finished())));
        if self.finished.len() > count {
            self.finished_at = now;
        } else if now.saturating_duration_since(self.finished_at) >= KEEP_FINISHED {
            self.finished.clear();
        }
        self.shown.clone_from(&rows);
        rows.extend(self.finished.iter().cloned());
        rows.sort_by_key(|&(started, _)| started);
        rows.into_iter().map(|(_, row)| row).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hotline_proto::types::HlPath;
    use hotline_server::{InMemoryFileProvider, Providers};

    use super::*;
    use crate::session::test_support;
    use crate::transfer::Start;

    #[tokio::test]
    async fn a_reply_runs_its_then_step() {
        let addr = test_support::start_server(Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());
        let mut tasks = Tasks::new(&client);
        let request = ClientSession::get_file_name_list(&tasks.client, HlPath::default());
        tasks.start(request, |listed, _, view, _| {
            view.news_count = usize::from(listed);
        });
        let step = tasks.next().await.unwrap();
        let mut view = View::default();
        step(&mut session, &mut view, &mut tasks);
        assert_eq!(view.news_count, 1);
    }

    #[tokio::test]
    async fn rows_keep_a_finished_transfer_for_a_moment() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"a");
        let providers = Providers {
            files: Arc::new(files),
            ..Providers::default()
        };
        let addr = test_support::start_server(providers).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = ClientSession::new("me", 0, client.encoding());
        session.download_dir = dir.path().to_path_buf();
        let mut tasks = Tasks::new(&client);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let _ = session
            .download_file(&client, "a.txt", "a.txt", Start::Ask, &tx)
            .run(&mut session)
            .await;
        let rows = tasks.rows(&mut session);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].cancellable);
        tokio::time::timeout(test_support::WAIT, rx.recv())
            .await
            .unwrap();
        while !session.running_transfers().is_empty() {
            assert_eq!(session.end_transfers(), []);
            tokio::task::yield_now().await;
        }

        let rows = tasks.rows(&mut session);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].label.starts_with("Downloading “a.txt”"));
        assert!(!rows[0].cancellable);
        tasks.finished_at -= KEEP_FINISHED;
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[test]
    fn a_finished_row_cannot_cancel() {
        let row = TaskRow {
            id: TaskId::Transfer(1),
            label: "Downloading".into(),
            cancellable: true,
        };
        assert!(!row.finished().cancellable);
    }
}
