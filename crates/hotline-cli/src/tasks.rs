//! The requests that run off the event loop, and the rows of the task list.

use std::time::{Duration, Instant};

use hotline_client::HotlineClient;
use tokio::task::{AbortHandle, JoinSet};

use crate::session::{ClientSession, Next, Request};
use crate::view::View;

/// How long the finished tasks stay in the task list after the last one
/// ends.
const KEEP_FINISHED: Duration = Duration::from_millis(500);

/// What a reply does to the session and the view.
pub type Step = Box<dyn FnOnce(&mut ClientSession, &mut View, &mut Tasks) + Send>;

/// What Enter does on a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// Cancel the task.
    Cancel,
    /// Cancel the task and end the connection.
    Disconnect,
}

/// A request as the task list shows it.
#[derive(Debug, Clone)]
pub struct Task {
    /// The text of the row, `None` for a request with no row.
    label: Option<Box<str>>,
    stop: Option<Stop>,
    /// `true` if the row stays for a moment after the request ends.
    keeps_row: bool,
}

impl Task {
    /// A request that only reads. The user can cancel it.
    pub fn read(label: impl Into<Box<str>>) -> Self {
        Self {
            label: Some(label.into()),
            stop: Some(Stop::Cancel),
            keeps_row: true,
        }
    }

    /// A request that changes the server. The user cannot cancel it.
    pub fn change(label: impl Into<Box<str>>) -> Self {
        Self {
            label: Some(label.into()),
            stop: None,
            keeps_row: true,
        }
    }

    /// A step of the login. A stop ends the connection.
    pub fn login(label: impl Into<Box<str>>) -> Self {
        Self {
            label: Some(label.into()),
            stop: Some(Stop::Disconnect),
            keeps_row: true,
        }
    }

    /// The start of a transfer, whose own row follows it.
    pub fn transfer(label: impl Into<Box<str>>) -> Self {
        Self {
            label: Some(label.into()),
            stop: None,
            keeps_row: false,
        }
    }

    /// A request with no row, such as a keep-alive.
    pub const fn unlisted() -> Self {
        Self {
            label: None,
            stop: None,
            keeps_row: true,
        }
    }
}

/// A task in the task list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskId {
    Request(u64),
    Transfer(u64),
}

/// One row of the task list.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: TaskId,
    pub label: String,
    /// What Enter does, `None` if the task cannot stop.
    pub stop: Option<Stop>,
    /// `true` while a transfer waits for its turn in `hlc`'s queue.
    pub queued: bool,
    /// The part that is done, from 0 to 1, when the size is known.
    pub ratio: Option<f64>,
}

impl TaskRow {
    /// The lines of the row: the label, and a gauge when the ratio is known.
    pub fn height(&self) -> u16 {
        u16::from(self.ratio.is_some()).saturating_add(1)
    }

    /// The row of this task after it ends well, with a full gauge.
    fn finished(self) -> Self {
        Self {
            stop: None,
            queued: false,
            ratio: Some(1.0),
            ..self
        }
    }
}

/// A request that waits for its reply.
struct Running {
    id: u64,
    /// When the user started the first request of the chain.
    started: Instant,
    task: Task,
    abort: AbortHandle,
}

/// The requests that run off the event loop.
pub struct Tasks {
    set: JoinSet<(u64, Step)>,
    running: Vec<Running>,
    last_id: u64,
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
            running: Vec::new(),
            last_id: 0,
            shown: Vec::new(),
            finished: Vec::new(),
            finished_at: Instant::now(),
            client: client.clone(),
        }
    }

    /// Run `request` as `task`, then give its value to `then`. Each request
    /// after it in a chain runs as the same task, in the same row.
    pub fn start<T: Send + 'static>(
        &mut self,
        task: Task,
        request: Request<T>,
        then: impl FnOnce(T, &mut ClientSession, &mut View, &mut Self) + Send + 'static,
    ) {
        self.last_id = self.last_id.wrapping_add(1);
        self.spawn(self.last_id, Instant::now(), task, request, then, false);
    }

    /// Run `request` as the request `id`, whose chain started at `started`.
    /// `failed` is `true` if an earlier reply of its chain showed an error.
    fn spawn<T: Send + 'static>(
        &mut self,
        id: u64,
        started: Instant,
        task: Task,
        request: Request<T>,
        then: impl FnOnce(T, &mut ClientSession, &mut View, &mut Self) + Send + 'static,
        failed: bool,
    ) {
        let next_task = task.clone();
        let keeps_row = task.keeps_row;
        let abort = self.set.spawn(async move {
            let apply = request.reply().await;
            let step: Step = Box::new(move |session, view, tasks| {
                let errors = session.error_count();
                let next = apply(session);
                // A chain that shows an error failed, and its row goes at once.
                let failed = failed || session.error_count() != errors;
                match next {
                    Next::Done(value) => {
                        if !keeps_row || failed {
                            tasks.forget(TaskId::Request(id));
                        }
                        then(value, session, view, tasks);
                    }
                    Next::Then(request) => {
                        tasks.spawn(id, started, next_task, request, then, failed);
                    }
                }
            });
            (id, step)
        });
        self.running.push(Running {
            id,
            started,
            task,
            abort,
        });
    }

    /// Run `request` as `task`.
    pub fn run(&mut self, task: Task, request: Request<()>) {
        self.start(task, request, |(), _, _, _| {});
    }

    /// Stop the request `id`. Its reply does nothing.
    pub fn cancel(&mut self, id: u64) {
        if let Some(pos) = self.running.iter().position(|r| r.id == id) {
            self.running.remove(pos).abort.abort();
        }
        self.forget(TaskId::Request(id));
    }

    /// Remove task `id` from the list when it ends, with no finished row.
    pub fn forget(&mut self, id: TaskId) {
        self.shown.retain(|(_, row)| row.id != id);
    }

    /// `true` while a request waits for its reply.
    pub const fn is_busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The step of the next reply. `None` when no request runs.
    pub async fn next(&mut self) -> Option<Step> {
        loop {
            match self.set.join_next().await? {
                Ok((id, step)) => {
                    // A request that was cancelled after its reply came has no row.
                    if let Some(pos) = self.running.iter().position(|r| r.id == id) {
                        self.running.remove(pos);
                        return Some(step);
                    }
                }
                Err(e) if e.is_cancelled() => {}
                Err(e) => std::panic::resume_unwind(e.into_panic()),
            }
        }
    }

    /// The rows of the task list: each transfer of `session`, each request,
    /// and each task that ended well, in the order the user started them.
    /// The finished rows go [`KEEP_FINISHED`] after the last task ends. A
    /// transfer that failed goes at once.
    pub fn rows(&mut self, session: &mut ClientSession) -> Vec<TaskRow> {
        for id in session.end_transfers() {
            self.forget(id);
        }
        let requests = self.running.iter().filter_map(|r| {
            let row = TaskRow {
                id: TaskId::Request(r.id),
                label: r.task.label.as_deref()?.to_string(),
                stop: r.task.stop,
                queued: false,
                ratio: None,
            };
            Some((r.started, row))
        });
        let mut rows: Vec<_> = session.transfer_rows().chain(requests).collect();
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
    use hotline_proto::types::HlPath;

    use super::*;
    use crate::session::test_support;

    async fn tasks() -> (Tasks, ClientSession) {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let session = ClientSession::new("me", 0, client.encoding());
        (Tasks::new(&client), session)
    }

    #[tokio::test]
    async fn a_reply_runs_its_then_step() {
        let (mut tasks, mut session) = tasks().await;
        let request = ClientSession::get_file_name_list(&tasks.client, HlPath::default());
        tasks.start(
            Task::read("Listing root folder..."),
            request,
            |listed, _, view, _| {
                view.news_count = usize::from(listed);
            },
        );
        assert!(tasks.is_busy());
        let step = tasks.next().await.unwrap();
        let mut view = View::default();
        step(&mut session, &mut view, &mut tasks);
        assert_eq!(view.news_count, 1);
        assert!(!tasks.is_busy());
    }

    #[tokio::test]
    async fn a_cancelled_request_gives_no_step() {
        let (mut tasks, mut session) = tasks().await;
        let request = ClientSession::get_file_name_list(&tasks.client, HlPath::default());
        tasks.start(
            Task::read("Listing root folder..."),
            request,
            |_, _, _, _| {},
        );
        tasks.cancel(1);
        assert!(!tasks.is_busy());
        assert!(tasks.next().await.is_none());
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[tokio::test]
    async fn a_request_cancelled_after_its_reply_gives_no_step() {
        let (mut tasks, _) = tasks().await;
        let request = ClientSession::get_file_name_list(&tasks.client, HlPath::default());
        tasks.start(
            Task::read("Listing root folder..."),
            request,
            |_, _, _, _| {},
        );
        while !tasks.running[0].abort.is_finished() {
            tokio::task::yield_now().await;
        }
        tasks.cancel(1);
        assert!(tasks.next().await.is_none());
    }

    #[tokio::test]
    async fn rows_keep_a_finished_request_for_a_moment() {
        let (mut tasks, mut session) = tasks().await;
        tasks.run(
            Task::read("Listing root folder..."),
            ClientSession::get_msgs(&tasks.client),
        );
        assert_eq!(tasks.rows(&mut session).len(), 1);
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);

        let rows = tasks.rows(&mut session);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Listing root folder...");
        assert_eq!(rows[0].stop, None);
        assert_eq!(rows[0].ratio, Some(1.0));
        tasks.finished_at -= KEEP_FINISHED;
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[tokio::test]
    async fn rows_keep_the_order_the_user_started_them() {
        let (mut tasks, mut session) = tasks().await;
        tasks.run(
            Task::read("Listing root folder..."),
            ClientSession::get_msgs(&tasks.client),
        );
        tasks.rows(&mut session);
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);
        tasks.run(
            Task::read("Transferring messages..."),
            ClientSession::get_msgs(&tasks.client),
        );
        let rows = tasks.rows(&mut session);
        let labels: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Listing root folder...", "Transferring messages..."]
        );
        assert_eq!(rows[0].stop, None);
    }

    #[tokio::test]
    async fn rows_leave_out_an_unlisted_request() {
        let (mut tasks, mut session) = tasks().await;
        tasks.run(Task::unlisted(), ClientSession::get_msgs(&tasks.client));
        assert!(tasks.is_busy());
        assert!(tasks.rows(&mut session).is_empty());
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[tokio::test]
    async fn rows_drop_a_failed_request_at_once() {
        let (mut tasks, mut session) = tasks().await;
        let missing = HlPath::new(["missing"]);
        tasks.start(
            Task::read("Listing folder “missing”"),
            ClientSession::get_file_name_list(&tasks.client, missing),
            |_, _, _, _| {},
        );
        assert_eq!(tasks.rows(&mut session).len(), 1);
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[tokio::test]
    async fn rows_drop_a_chain_whose_first_request_failed() {
        let (mut tasks, mut session) = tasks().await;
        let client = tasks.client.clone();
        let missing = HlPath::new(["missing"]);
        let request = ClientSession::get_file_name_list(&tasks.client, missing)
            .and_then(move |_, _| ClientSession::get_msgs(&client));
        tasks.run(Task::read("Listing folder “missing”"), request);
        assert_eq!(tasks.rows(&mut session).len(), 1);
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);
        assert_eq!(tasks.rows(&mut session).len(), 1);
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[tokio::test]
    async fn rows_drop_the_start_of_a_transfer_at_once() {
        let (mut tasks, mut session) = tasks().await;
        tasks.run(
            Task::transfer("Downloading “a”"),
            ClientSession::get_msgs(&tasks.client),
        );
        assert_eq!(tasks.rows(&mut session).len(), 1);
        let step = tasks.next().await.unwrap();
        step(&mut session, &mut View::default(), &mut tasks);
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[tokio::test]
    async fn rows_drop_a_cancelled_request_at_once() {
        let (mut tasks, mut session) = tasks().await;
        tasks.run(
            Task::read("Listing root folder..."),
            ClientSession::get_msgs(&tasks.client),
        );
        assert_eq!(tasks.rows(&mut session).len(), 1);
        tasks.cancel(1);
        assert!(tasks.rows(&mut session).is_empty());
    }

    #[test]
    fn a_finished_row_cannot_stop_and_has_a_full_gauge() {
        let row = TaskRow {
            id: TaskId::Transfer(1),
            label: "Downloading".into(),
            stop: Some(Stop::Cancel),
            queued: false,
            ratio: None,
        };
        let finished = row.finished();
        assert_eq!(finished.stop, None);
        assert_eq!(finished.ratio, Some(1.0));
    }

    #[tokio::test]
    async fn rows_show_a_request_at_once() {
        let (mut tasks, mut session) = tasks().await;
        tasks.run(
            Task::login("Logging in..."),
            ClientSession::get_msgs(&tasks.client),
        );
        assert_eq!(
            tasks.rows(&mut session),
            [TaskRow {
                id: TaskId::Request(1),
                label: "Logging in...".into(),
                stop: Some(Stop::Disconnect),
                queued: false,
                ratio: None,
            }]
        );
    }
}
