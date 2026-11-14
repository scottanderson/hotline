//! The borders between panes that the user drags to resize them.

use super::{Tab, View};
use crate::ui::{self, Areas, Side};

/// The fewest columns or rows of a pane: two borders and one cell.
const MIN_PANE: u16 = 3;

/// The fewest columns of the body beside the right column.
const MIN_BODY: u16 = 20;

/// A border between two panes that the user can drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Divider {
    /// The left border of the right column.
    Side,
    /// The right border of the Threaded News folder tree.
    NewsFolders,
    /// The bottom border of the Threaded News thread list.
    NewsThreads,
}

/// The pane sizes that the user sets with a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Splits {
    /// The columns of the right column, which holds the user list.
    pub side: u16,
    /// The columns of the folder tree, or `None` for 30% of the body.
    pub news_folders: Option<u16>,
    /// The rows of the thread list, or `None` for half of the body.
    pub news_threads: Option<u16>,
}

impl Default for Splits {
    fn default() -> Self {
        Self {
            side: 24,
            news_folders: None,
            news_threads: None,
        }
    }
}

/// `n` kept between `min` and `max`. `min` wins when `max` is less.
fn bound(n: u16, min: u16, max: u16) -> u16 {
    n.min(max).max(min)
}

impl View {
    /// The right column, or `None` while it hides.
    pub fn side(&self) -> Option<Side> {
        self.shows_users().then_some(Side {
            width: self.splits.side,
        })
    }

    /// `true` if the tab shows the Threaded News panes.
    pub fn shows_news_panes(&self) -> bool {
        self.tab == Tab::Threaded && self.may(self.tab.enabled_by())
    }

    /// The divider at `x`, `y`.
    pub(super) fn divider_at(&self, areas: &Areas, x: u16, y: u16) -> Option<Divider> {
        let users = areas.users;
        if self.side().is_some() && x == users.x && (users.y..users.bottom()).contains(&y) {
            return Some(Divider::Side);
        }
        if !self.shows_news_panes() {
            return None;
        }
        let body = areas.body;
        let [_, threads, preview] = ui::news_areas(body, self.splits);
        if y == preview.y && (threads.x..threads.right()).contains(&x) {
            Some(Divider::NewsThreads)
        } else if x == threads.x && (body.y..body.bottom()).contains(&y) {
            Some(Divider::NewsFolders)
        } else {
            None
        }
    }

    /// Move `divider` to `x`, `y`.
    pub(super) fn drag(&mut self, divider: Divider, areas: &Areas, x: u16, y: u16) {
        let body = areas.body;
        match divider {
            Divider::Side => {
                let screen = areas.screen;
                let width = screen.right().saturating_sub(x);
                let max = screen.width.saturating_sub(MIN_BODY - 1);
                self.splits.side = bound(width, MIN_PANE, max);
            }
            Divider::NewsFolders => {
                let width = x.saturating_sub(body.x).saturating_add(1);
                let max = body.width.saturating_sub(MIN_PANE - 1);
                self.splits.news_folders = Some(bound(width, MIN_PANE, max));
            }
            Divider::NewsThreads => {
                let height = y.saturating_sub(body.y).saturating_add(1);
                let max = body.height.saturating_sub(MIN_PANE - 1);
                self.splits.news_threads = Some(bound(height, MIN_PANE, max));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use hotline_proto::Access;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 80, 24);

    fn areas(view: &View) -> Areas {
        ui::areas(SCREEN, view.input().map(|_| ""), &[], view.side(), false)
    }

    fn mouse(view: &mut View, kind: MouseEventKind, column: u16, row: u16) {
        let event = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let _ = view.handle_mouse(event, &areas(view), &[], &[], Instant::now());
    }

    /// Drag with the left button from `from` to `to`.
    fn drag(view: &mut View, from: (u16, u16), to: (u16, u16)) {
        mouse(
            view,
            MouseEventKind::Down(MouseButton::Left),
            from.0,
            from.1,
        );
        mouse(view, MouseEventKind::Drag(MouseButton::Left), to.0, to.1);
        mouse(view, MouseEventKind::Up(MouseButton::Left), to.0, to.1);
    }

    #[test]
    fn a_drag_of_the_right_column_border_resizes_it() {
        let mut view = View::default();
        let border = areas(&view).users.x;
        drag(&mut view, (border, 5), (border - 10, 5));
        assert_eq!(view.splits.side, 34);
        assert_eq!(areas(&view).users.x, border - 10);
    }

    #[test]
    fn a_drag_keeps_each_pane_at_its_least_size() {
        let mut view = View::default();
        let border = areas(&view).users.x;
        drag(&mut view, (border, 5), (0, 5));
        assert_eq!(areas(&view).body.width, MIN_BODY);
        let border = areas(&view).users.x;
        drag(&mut view, (border, 5), (79, 5));
        assert_eq!(view.splits.side, MIN_PANE);
    }

    #[test]
    fn a_drag_after_the_button_comes_up_does_nothing() {
        let mut view = View::default();
        let border = areas(&view).users.x;
        drag(&mut view, (border, 5), (border - 10, 5));
        mouse(&mut view, MouseEventKind::Drag(MouseButton::Left), 10, 5);
        assert_eq!(view.splits.side, 34);
    }

    #[test]
    fn a_drag_of_a_news_border_resizes_the_news_panes() {
        let mut view = View {
            tab: Tab::Threaded,
            access: Access::all(),
            ..View::default()
        };
        let body = areas(&view).body;
        let [_, threads, preview] = ui::news_areas(body, view.splits);
        drag(&mut view, (threads.x, 3), (40, 3));
        drag(&mut view, (60, preview.y), (60, 6));
        let [folders, threads, preview] = ui::news_areas(body, view.splits);
        assert_eq!((folders.right() - 1, threads.x), (40, 40));
        assert_eq!((threads.bottom() - 1, preview.y), (6, 6));
    }

    #[test]
    fn the_news_borders_do_not_drag_without_the_news_privilege() {
        let mut view = View {
            tab: Tab::Threaded,
            ..View::default()
        };
        let body = areas(&view).body;
        let [_, threads, _] = ui::news_areas(body, view.splits);
        drag(&mut view, (threads.x, 3), (40, 3));
        assert_eq!(view.splits, Splits::default());
    }
}
