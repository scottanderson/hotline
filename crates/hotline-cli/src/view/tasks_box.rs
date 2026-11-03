//! The tasks box in the right column.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use super::{contains, Action, Focus, Hint, View};
use crate::tasks::{Stop, TaskId, TaskRow};
use crate::ui;

impl View {
    /// Show `rows` in the tasks box. The selection stays on its task. With
    /// no task, the focus leaves the box.
    pub fn set_tasks(&mut self, rows: Vec<TaskRow>) {
        let id = self.selected_task().map(|row| row.id);
        self.task_selected = id
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .unwrap_or(self.task_selected)
            .min(rows.len().saturating_sub(1));
        self.tasks = rows;
        if self.focus == Focus::Tasks && !self.shows_tasks() {
            self.focus = Focus::main(self.tab);
        }
    }

    /// `true` while a task runs, which shows the tasks box.
    pub const fn shows_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// The lines of all the rows of the tasks box.
    pub fn task_lines(&self) -> usize {
        self.tasks.iter().map(|row| usize::from(row.height())).sum()
    }

    /// The selected row of the tasks box.
    pub fn selected_task(&self) -> Option<&TaskRow> {
        self.tasks.get(self.task_selected)
    }

    /// The index of the selected row of the tasks box.
    pub const fn task_selected(&self) -> usize {
        self.task_selected
    }

    /// The row of the tasks box at `x`, `y`.
    pub(super) fn task_at(&self, list: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(list);
        if !self.shows_tasks() || !contains(inner, x, y) {
            return None;
        }
        let mut bottom = inner.y;
        self.tasks
            .iter()
            .enumerate()
            .skip(self.tasks_offset)
            .find_map(|(index, row)| {
                bottom = bottom.saturating_add(row.height());
                (y < bottom).then_some(index)
            })
    }

    pub(super) fn tasks_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Tab | KeyCode::Esc => self.focus = Focus::main(self.tab),
            KeyCode::Up => self.task_selected = self.task_selected.saturating_sub(1),
            KeyCode::Down => {
                self.task_selected = self
                    .task_selected
                    .saturating_add(1)
                    .min(self.tasks.len().saturating_sub(1));
            }
            KeyCode::Enter => match self.selected_task().map(|row| (row.id, row.stop)) {
                Some((id, Some(Stop::Cancel))) => return Action::CancelTask(id),
                Some((id, Some(Stop::Disconnect))) => return Action::StopLogin(id),
                _ => {}
            },
            KeyCode::Char('s') => {
                if let Some(TaskId::Transfer(id)) = self.queued_task() {
                    return Action::StartTransfer(id);
                }
            }
            _ => {}
        }
        Action::None
    }

    /// The selected task, if it waits for its turn in `hlc`'s queue.
    fn queued_task(&self) -> Option<TaskId> {
        self.selected_task()
            .filter(|row| row.queued)
            .map(|row| row.id)
    }

    /// The keys of the tasks box.
    pub(super) fn tasks_hints(&self) -> Vec<Hint> {
        let stop = self.selected_task().and_then(|row| row.stop);
        let stop = match stop {
            Some(Stop::Disconnect) => Hint::new("Enter", "disconnect"),
            _ => Hint::new("Enter", "cancel").only_if(stop.is_some()),
        };
        let main = self.main_name();
        vec![
            Hint::new("↑↓", "select"),
            stop,
            Hint::new("s", "start").only_if(self.queued_task().is_some()),
            Hint::new("Tab", main),
            Hint::new("Esc", main),
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use hotline_proto::Access;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    use super::*;
    use crate::tasks::TaskId;
    use crate::view::Tab;

    fn row(id: TaskId, cancellable: bool) -> TaskRow {
        TaskRow {
            id,
            label: "a task".into(),
            stop: cancellable.then_some(Stop::Cancel),
            queued: false,
            ratio: None,
        }
    }

    fn press(view: &mut View, code: KeyCode) -> Action {
        view.handle_key(KeyEvent::from(code), &[], &[])
    }

    fn alt_j(view: &mut View) -> Action {
        let key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);
        view.handle_key(key, &[], &[])
    }

    fn view_with_tasks() -> View {
        let mut view = View {
            access: Access::all(),
            ..View::default()
        };
        view.set_tasks(vec![
            row(TaskId::Transfer(1), true),
            row(TaskId::Request(4), false),
        ]);
        view
    }

    #[test]
    fn alt_j_focuses_the_tasks_box_only_while_it_shows() {
        let mut view = View::default();
        let _ = alt_j(&mut view);
        assert_eq!(view.focus, Focus::Input);

        let mut view = view_with_tasks();
        let _ = alt_j(&mut view);
        assert_eq!(view.focus, Focus::Tasks);
        let _ = press(&mut view, KeyCode::Esc);
        assert_eq!(view.focus, Focus::Input);
    }

    #[test]
    fn tab_goes_from_the_user_list_to_the_tasks_box() {
        let mut view = View {
            focus: Focus::Users,
            ..View::default()
        };
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::Input);

        let mut view = view_with_tasks();
        view.focus = Focus::Users;
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::Tasks);
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::Input);
    }

    #[test]
    fn enter_cancels_only_a_task_that_can_stop() {
        let mut view = view_with_tasks();
        view.focus = Focus::Tasks;
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Action::CancelTask(TaskId::Transfer(1))
        );
        let _ = press(&mut view, KeyCode::Down);
        let _ = press(&mut view, KeyCode::Down);
        assert_eq!(view.selected_task().unwrap().id, TaskId::Request(4));
        let hint = view.tasks_hints()[1];
        assert!(!hint.on);
        assert_eq!(press(&mut view, KeyCode::Enter), Action::None);
    }

    #[test]
    fn enter_on_a_login_step_disconnects() {
        let mut view = View::default();
        let mut login = row(TaskId::Request(1), true);
        login.stop = Some(Stop::Disconnect);
        view.set_tasks(vec![login]);
        view.focus = Focus::Tasks;
        assert_eq!(view.tasks_hints()[1].does, "disconnect");
        assert_eq!(
            press(&mut view, KeyCode::Enter),
            Action::StopLogin(TaskId::Request(1))
        );
    }

    #[test]
    fn s_starts_a_queued_transfer() {
        let mut view = View::default();
        let mut queued = row(TaskId::Transfer(2), true);
        queued.queued = true;
        view.set_tasks(vec![row(TaskId::Transfer(1), true), queued]);
        view.focus = Focus::Tasks;
        assert!(!view.tasks_hints()[2].on);
        assert_eq!(press(&mut view, KeyCode::Char('s')), Action::None);
        let _ = press(&mut view, KeyCode::Down);
        assert!(view.tasks_hints()[2].on);
        assert_eq!(
            press(&mut view, KeyCode::Char('s')),
            Action::StartTransfer(2)
        );
    }

    #[test]
    fn set_tasks_keeps_the_selected_task_and_frees_the_focus() {
        let mut view = view_with_tasks();
        view.focus = Focus::Tasks;
        let _ = press(&mut view, KeyCode::Down);
        view.set_tasks(vec![
            row(TaskId::Request(2), true),
            row(TaskId::Transfer(1), true),
            row(TaskId::Request(4), false),
        ]);
        assert_eq!(view.selected_task().unwrap().id, TaskId::Request(4));

        view.set_tasks(Vec::new());
        assert_eq!(view.focus, Focus::Input);
    }

    #[test]
    fn a_click_on_a_task_selects_it_and_focuses_the_box() {
        let mut view = view_with_tasks();
        let areas = ui::areas(
            Rect::new(0, 0, 80, 24),
            Some(""),
            &[],
            Some(ui::Side::USERS),
            false,
            2,
        );
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: areas.tasks.x + 1,
            row: areas.tasks.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        let _ = view.handle_mouse(mouse, &areas, &[], &[], Instant::now());
        assert_eq!(view.focus, Focus::Tasks);
        assert_eq!(view.selected_task().unwrap().id, TaskId::Request(4));
    }

    #[test]
    fn a_click_on_a_gauge_selects_its_task() {
        let mut view = view_with_tasks();
        let mut rows = view.tasks.clone();
        rows[0].ratio = Some(0.5);
        view.set_tasks(rows);
        let areas = ui::areas(
            Rect::new(0, 0, 80, 24),
            Some(""),
            &[],
            Some(ui::Side::USERS),
            false,
            view.task_lines(),
        );
        let inner = ui::inner(areas.tasks);
        assert_eq!(view.task_at(areas.tasks, inner.x, inner.y + 1), Some(0));
        assert_eq!(view.task_at(areas.tasks, inner.x, inner.y + 2), Some(1));
    }

    #[test]
    fn the_tasks_box_shows_on_a_tab_without_the_user_list() {
        let mut view = view_with_tasks();
        view.tab = Tab::Files;
        view.focus = Focus::Files;
        assert_eq!(view.side().map(|side| side.users), Some(false));
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::Tasks);
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::Files);

        let areas = ui::areas(Rect::new(0, 0, 80, 24), None, &[], view.side(), false, 2);
        assert_eq!(areas.users.height, 0);
        assert_eq!(areas.tasks.height, areas.body.height);
    }

    #[test]
    fn tab_from_the_news_preview_moves_to_the_tasks_box() {
        let mut view = view_with_tasks();
        view.tab = Tab::Threaded;
        view.focus = Focus::NewsPreview;
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::Tasks);
        let _ = press(&mut view, KeyCode::Tab);
        assert_eq!(view.focus, Focus::NewsFolders);
    }
}
