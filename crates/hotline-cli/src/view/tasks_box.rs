//! The task list on the Tasks tab.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use super::{contains, Action, Hint, Tab, View};
use crate::tasks::{Stop, TaskId, TaskRow};
use crate::ui;

impl View {
    /// Show `rows` in the task list. The selection stays on its task.
    pub fn set_tasks(&mut self, rows: Vec<TaskRow>) {
        let id = self.selected_task().map(|row| row.id);
        self.task_selected = id
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .unwrap_or(self.task_selected)
            .min(rows.len().saturating_sub(1));
        self.tasks = rows;
    }

    /// The lines of all the rows of the task list.
    pub fn task_lines(&self) -> usize {
        self.tasks.iter().map(|row| usize::from(row.height())).sum()
    }

    /// The selected row of the task list.
    pub fn selected_task(&self) -> Option<&TaskRow> {
        self.tasks.get(self.task_selected)
    }

    /// The index of the selected row of the task list.
    pub const fn task_selected(&self) -> usize {
        self.task_selected
    }

    /// The row of the task list in `list` at `x`, `y`.
    pub(super) fn task_at(&self, list: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(list);
        if self.tab != Tab::Tasks || !contains(inner, x, y) {
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
            KeyCode::Esc => return self.set_tab(Tab::Chat),
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

    /// The keys of the task list.
    pub(super) fn tasks_hints(&self) -> Vec<Hint> {
        let stop = self.selected_task().and_then(|row| row.stop);
        let stop = match stop {
            Some(Stop::Disconnect) => Hint::new("Enter", "disconnect"),
            _ => Hint::new("Enter", "cancel").only_if(stop.is_some()),
        };
        vec![
            Hint::new("↑↓", "select"),
            stop,
            Hint::new("s", "start").only_if(self.queued_task().is_some()),
            Hint::new("Alt-T", "back"),
            Hint::new("Esc", "chat"),
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
    use crate::view::{Focus, Tab};

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

    fn alt_t(view: &mut View) -> Action {
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT);
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
        let _ = view.set_tab(Tab::Tasks);
        view
    }

    #[test]
    fn alt_t_shows_the_tasks_and_goes_back() {
        let mut view = View::default();
        let _ = view.set_tab(Tab::Files);
        let _ = alt_t(&mut view);
        assert_eq!((view.tab, view.focus), (Tab::Tasks, Focus::Tasks));
        let _ = alt_t(&mut view);
        assert_eq!((view.tab, view.focus), (Tab::Files, Focus::Files));
    }

    #[test]
    fn alt_t_goes_back_to_chat_when_the_tab_is_gone() {
        let mut view = View::default();
        let mut tabs = view.tabs.clone();
        tabs.push((Tab::Room(7), "room".into()));
        view.set_tabs(tabs.clone());
        let _ = view.set_tab(Tab::Room(7));
        let _ = alt_t(&mut view);
        tabs.pop();
        view.set_tabs(tabs);
        let _ = alt_t(&mut view);
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn the_tasks_tab_shows_with_no_task() {
        let mut view = View::default();
        let _ = alt_t(&mut view);
        assert_eq!((view.tab, view.focus), (Tab::Tasks, Focus::Tasks));
        view.set_tasks(Vec::new());
        assert_eq!((view.tab, view.focus), (Tab::Tasks, Focus::Tasks));
        let _ = press(&mut view, KeyCode::Esc);
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn enter_cancels_only_a_task_that_can_stop() {
        let mut view = view_with_tasks();
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
        let _ = view.set_tab(Tab::Tasks);
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
        let _ = view.set_tab(Tab::Tasks);
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
    fn set_tasks_keeps_the_selected_task() {
        let mut view = view_with_tasks();
        let _ = press(&mut view, KeyCode::Down);
        view.set_tasks(vec![
            row(TaskId::Request(2), true),
            row(TaskId::Transfer(1), true),
            row(TaskId::Request(4), false),
        ]);
        assert_eq!(view.selected_task().unwrap().id, TaskId::Request(4));
    }

    fn areas(view: &View) -> ui::Areas {
        ui::areas(Rect::new(0, 0, 80, 24), None, &[], view.side(), false)
    }

    #[test]
    fn a_click_on_a_task_selects_it() {
        let mut view = view_with_tasks();
        let areas = areas(&view);
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: areas.body.x + 1,
            row: areas.body.y + 2,
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
        let areas = areas(&view);
        let inner = ui::inner(areas.body);
        assert_eq!(view.task_at(areas.body, inner.x, inner.y + 1), Some(0));
        assert_eq!(view.task_at(areas.body, inner.x, inner.y + 2), Some(1));
    }

    #[test]
    fn the_tasks_tab_takes_the_full_width() {
        let view = view_with_tasks();
        assert_eq!(view.side(), None);
        assert_eq!(areas(&view).body.width, 80);
    }
}
