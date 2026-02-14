use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_login::ShutdownHandle;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use textwrap::wrap;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use crate::key_hint;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::style::user_message_style;
use crate::wrapping::word_wrap_lines;

#[derive(Debug, Clone)]
pub(crate) enum ChatGptAddAccountStatus {
    Pending,
    Success {
        active_account_display: Option<String>,
    },
    Cancelled,
    Failed {
        message: String,
    },
}

#[derive(Debug)]
pub(crate) struct ChatGptAddAccountSharedState {
    status: Mutex<ChatGptAddAccountStatus>,
    cancelled_by_user: AtomicBool,
}

impl ChatGptAddAccountSharedState {
    pub(crate) fn new() -> Self {
        Self {
            status: Mutex::new(ChatGptAddAccountStatus::Pending),
            cancelled_by_user: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_success(&self, active_account_display: Option<String>) {
        self.set_status(ChatGptAddAccountStatus::Success {
            active_account_display,
        });
    }

    pub(crate) fn set_cancelled(&self) {
        self.set_status(ChatGptAddAccountStatus::Cancelled);
    }

    pub(crate) fn set_failed(&self, message: String) {
        self.set_status(ChatGptAddAccountStatus::Failed { message });
    }

    pub(crate) fn status(&self) -> ChatGptAddAccountStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_status(&self, status: ChatGptAddAccountStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    }

    pub(crate) fn mark_cancelled_by_user(&self) {
        self.cancelled_by_user.store(true, Ordering::SeqCst);
    }

    pub(crate) fn cancelled_by_user(&self) -> bool {
        self.cancelled_by_user.load(Ordering::SeqCst)
    }
}

pub(crate) struct ChatGptAddAccountView {
    auth_url: String,
    shared_state: Arc<ChatGptAddAccountSharedState>,
    shutdown_handle: ShutdownHandle,
    complete: bool,
}

impl ChatGptAddAccountView {
    pub(crate) fn new(
        auth_url: String,
        shared_state: Arc<ChatGptAddAccountSharedState>,
        shutdown_handle: ShutdownHandle,
    ) -> Self {
        Self {
            auth_url,
            shared_state,
            shutdown_handle,
            complete: false,
        }
    }

    fn content_lines(&self, width: u16) -> Vec<Line<'static>> {
        let usable_width = width.max(1) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from("Add ChatGPT account".bold()));
        for line in wrap(
            "Complete sign-in in your browser to add another ChatGPT account. The newly added account becomes active.",
            usable_width,
        ) {
            lines.push(Line::from(line.into_owned().dim()));
        }
        lines.push(Line::from(""));

        match self.shared_state.status() {
            ChatGptAddAccountStatus::Pending => {
                lines.push(Line::from("Waiting for login to complete..."));
                lines.push(Line::from(""));
            }
            ChatGptAddAccountStatus::Success {
                active_account_display,
            } => {
                lines.push(Line::from("Login complete.".green()));
                if let Some(display) = active_account_display {
                    lines.push(Line::from(format!("Active account: {display}")));
                }
                lines.push(Line::from(""));
            }
            ChatGptAddAccountStatus::Cancelled => {
                lines.push(Line::from("Login cancelled.".dim()));
                lines.push(Line::from(""));
            }
            ChatGptAddAccountStatus::Failed { message } => {
                lines.push(Line::from("Login failed.".red()));
                for line in wrap(message.trim(), usable_width) {
                    lines.push(Line::from(line.into_owned()));
                }
                lines.push(Line::from(""));
            }
        }

        lines.push(Line::from(vec!["Open:".dim()]));
        let url_line = Line::from(vec![self.auth_url.clone().cyan().underlined()]);
        lines.extend(word_wrap_lines(vec![url_line], usable_width));

        lines
    }

    fn hint_line(&self) -> Line<'static> {
        let action = match self.shared_state.status() {
            ChatGptAddAccountStatus::Pending => "cancel",
            ChatGptAddAccountStatus::Success { .. }
            | ChatGptAddAccountStatus::Cancelled
            | ChatGptAddAccountStatus::Failed { .. } => "close",
        };
        Line::from(vec![
            "Press ".into(),
            key_hint::plain(KeyCode::Esc).into(),
            format!(" to {action}").into(),
        ])
    }
}

impl BottomPaneView for ChatGptAddAccountView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if let KeyEvent {
            code: KeyCode::Esc, ..
        } = key_event
        {
            self.on_ctrl_c();
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if matches!(self.shared_state.status(), ChatGptAddAccountStatus::Pending) {
            self.shared_state.mark_cancelled_by_user();
            self.shutdown_handle.shutdown();
        }

        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

impl crate::render::renderable::Renderable for ChatGptAddAccountView {
    fn desired_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4).max(1);
        let content_lines = self.content_lines(content_width);
        content_lines.len() as u16 + 3
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        Block::default()
            .style(user_message_style())
            .render(area, buf);

        let [content_area, hint_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        let inner = content_area.inset(Insets::vh(1, 2));
        let content_width = inner.width.max(1);
        let lines = self.content_lines(content_width);
        Paragraph::new(lines).render(inner, buf);

        if hint_area.height > 0 {
            let hint_area = Rect {
                x: hint_area.x.saturating_add(2),
                y: hint_area.y,
                width: hint_area.width.saturating_sub(2),
                height: hint_area.height,
            };
            self.hint_line().dim().render(hint_area, buf);
        }
    }
}
