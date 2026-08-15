//! The approval dialog (D81): the permission gate's three-option prompt and
//! AskUserQuestion's select, which share one modal queue and one key surface.
//! Owns no state; `impl super::Chat`.
//!
//! A permission prompt shows *what* it is about to do — the command, or a
//! dry-run diff computed without touching the file — above three options: yes,
//! yes-for-this-session, and a no that opens a feedback row. The session option
//! is offered only when the permission engine could derive a rule that really
//! silences the gate, so the promise it makes is one the gate keeps; the
//! feedback the refusal collects travels to the model inside the
//! `<permission_error>`, so a denial is a direction rather than only a wall.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::ui::{AskKind, AskPreview};

/// A dialog that appears under a keystroke already on its way must not answer
/// it. The first moments of a permission prompt ignore confirmations, so a
/// stray Enter approves nothing (CC's confirm delay).
pub(crate) const ASK_CONFIRM_GUARD: std::time::Duration = std::time::Duration::from_millis(400);

/// Bounded preview height, collapsed. A long diff would push the question and
/// the options themselves off the screen — ctrl+e lifts the bound.
const ASK_DIFF_ROWS: usize = 12;
/// Bounded command height, collapsed (heredocs and `&&` chains run long).
const ASK_COMMAND_ROWS: usize = 6;

impl super::Chat {
    /// Drains the permission channel (one at a time: a new request is only accepted when none is pending).
    pub fn drain_asks(&mut self) -> bool {
        if self.pending_ask.is_none()
            && let Ok(request) = self.asks_rx.try_recv()
        {
            self.ask_focus = 0;
            self.ask_other.clear();
            self.ask_expanded = false;
            // The guard runs from the moment the dialog exists, which is the
            // moment the keystroke already in flight would land on it.
            self.ask_opened_at = Some(std::time::Instant::now());
            self.pending_ask = Some(request);
            // The turn is blocked until this is answered, and the user may well
            // be looking somewhere else by now (D79).
            self.notify.attention(Attention::WaitingPermission);
            self.notify.set_title(Title::WaitingPermission);
            return true;
        }
        false
    }

    /// Settles the permission dialogs a dead turn left behind (D80).
    ///
    /// The dialog and the turn used to have separate lifetimes: an interrupt
    /// killed the task awaiting the answer and left the question on screen, so
    /// the footer went on saying `Waiting for permission…` and every 1-9 the
    /// user pressed answered a corpse — the reply went into a dropped oneshot
    /// and nothing happened. Cancelling here closes both ends: an explicit
    /// `Cancel` on the wire (the receiver reads it as a deny — fail closed),
    /// the requests still queued behind it emptied the same way, and one dim
    /// line in the flow where the dialog was.
    ///
    /// `dead_only` keeps a background agent out of the foreground turn's
    /// cleanup: subagents share this modal queue, so at turn end the only
    /// requests that belong to the turn that just ended are the ones whose
    /// receiver is already gone. An explicit interrupt takes everything,
    /// because that is what the user asked for.
    pub(crate) fn cancel_asks(&mut self, dead_only: bool) -> bool {
        let mut cancelled = false;
        let settle = |ask: crate::ui::AskRequest| {
            let _ = ask.1.send(crate::ui::DialogAction::Cancel);
        };
        if let Some(ask) = self.pending_ask.take() {
            if dead_only && !ask.1.is_closed() {
                self.pending_ask = Some(ask);
            } else {
                settle(ask);
                cancelled = true;
            }
        }
        // A request still in the channel has no row of its own yet; it is
        // drained here so it cannot surface as a dialog for a turn that is
        // already gone. Live ones go back in the same order they came out.
        let mut keep = Vec::new();
        while let Ok(ask) = self.asks_rx.try_recv() {
            if dead_only && !ask.1.is_closed() {
                keep.push(ask);
            } else {
                settle(ask);
                cancelled = true;
            }
        }
        for ask in keep {
            let _ = self.asks.send(ask);
        }
        if cancelled {
            self.reset_ask_state();
            self.drop_empty_stream_message();
            self.push_user_line(ASK_CANCELLED_TEXT.to_string());
            // The title still announced a question nobody could answer.
            self.notify_idle();
            self.dirty = true;
        }
        cancelled
    }

    fn reset_ask_state(&mut self) {
        self.ask_focus = 0;
        self.ask_other.clear();
        self.ask_expanded = false;
        self.ask_opened_at = None;
    }

    /// Whether a confirmation landing right now is type-ahead rather than an
    /// answer. Permission prompts only: a wrong AskUserQuestion option costs a
    /// round trip, a wrong approval runs a command.
    fn confirm_guarded(&self, now: std::time::Instant) -> bool {
        self.pending_ask
            .as_ref()
            .is_some_and(|(request, _)| request.kind == AskKind::Permission)
            && self
                .ask_opened_at
                .is_some_and(|at| now.duration_since(at) < ASK_CONFIRM_GUARD)
    }

    /// Dialog key input (Select semantics): digits/Enter confirm, ↑/↓ move the
    /// focus, Esc denies, shift+tab takes the session option directly and
    /// ctrl+e expands the preview. Typing goes straight into the free-text row
    /// when the focus is on it. Returns whether it was consumed.
    ///
    /// Modifier-carrying chars other than ctrl+e are NOT consumed: crossterm
    /// reports ctrl+c as `Char('c')` + CONTROL, so swallowing them here turned
    /// the interrupt (and every readline chord) into literal letters inside the
    /// free-text input.
    ///
    /// Real-clock version; the type-ahead guard needs an injectable now, so
    /// production always goes through [`Chat::ask_key_at`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ask_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.ask_key_at(code, modifiers, std::time::Instant::now())
    }

    /// Dialog key input at `now`; semantics in [`Chat::ask_key`].
    pub fn ask_key_at(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        let Some((request, _)) = &self.pending_ask else {
            return false;
        };
        let permission = request.kind == AskKind::Permission;
        if let KeyCode::Char(c) = code
            && modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            // ctrl+e belongs to the dialog only while one is open; elsewhere it
            // stays the readline end-of-line it has always been.
            if c == 'e' && modifiers.contains(KeyModifiers::CONTROL) && request.preview.is_some() {
                self.ask_expanded = !self.ask_expanded;
                self.dirty = true;
                return true;
            }
            return false;
        }
        let options_len = request.options.len();
        let free_text = request.free_text;
        let total = options_len + usize::from(free_text && !permission);
        let in_other = free_text && self.ask_focus >= options_len;
        match code {
            KeyCode::Char(c) if in_other && !c.is_control() => {
                self.ask_other.push(c);
                true
            }
            KeyCode::Backspace if in_other => {
                self.ask_other.pop();
                true
            }
            KeyCode::Enter if in_other => {
                let text = std::mem::take(&mut self.ask_other);
                self.submit_ask_answer(text);
                true
            }
            // shift+tab is CC's shortcut onto the session option, from anywhere
            // in the dialog. Without one offered it does nothing: the dialog
            // owns the key while it is open, and cycling the permission mode
            // behind an unanswered question is not what the press meant.
            KeyCode::BackTab if permission => {
                if let Some(index) = self.session_option() {
                    self.choose_ask_option(index);
                }
                true
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                if self.confirm_guarded(now) {
                    return true;
                }
                let index = (c as u8 - b'1') as usize;
                if index < total {
                    self.ask_focus = index;
                    if !(index == options_len && free_text) {
                        self.choose_ask_option(index);
                    }
                }
                true
            }
            // A permission prompt's feedback row is a committed state: the
            // refusal is already made, and the only way out is Esc. An
            // AskUserQuestion's Other row is still one option among several,
            // so arrows keep walking the list from it.
            KeyCode::Up if !(permission && in_other) => {
                if self.ask_focus > 0 {
                    self.ask_focus -= 1;
                }
                true
            }
            KeyCode::Down if !(permission && in_other) => {
                if self.ask_focus + 1 < total {
                    self.ask_focus += 1;
                }
                true
            }
            KeyCode::Enter => {
                if self.confirm_guarded(now) {
                    return true;
                }
                let focus = self.ask_focus;
                if focus >= options_len && free_text {
                    let text = std::mem::take(&mut self.ask_other);
                    self.submit_ask_answer(text);
                } else {
                    self.choose_ask_option(focus);
                }
                true
            }
            // Esc denies plainly, from the option list and from the feedback row
            // alike: the user is leaving, not composing.
            KeyCode::Esc => {
                let Some((request, tx)) = self.pending_ask.take() else {
                    return true;
                };
                if request.kind == AskKind::Permission {
                    self.push_ask_message(ASK_RECEIPT_NO.to_string());
                } else if request.free_text {
                    self.push_ask_message(ASK_DECLINED_TEXT.to_string());
                }
                let _ = tx.send(DialogAction::Cancel);
                self.reset_ask_state();
                true
            }
            _ => false,
        }
    }

    /// Index of the "don't ask again this session" option, when it is offered.
    fn session_option(&self) -> Option<usize> {
        self.pending_ask
            .as_ref()
            .and_then(|(request, _)| request.session_option())
    }

    /// Click on a dialog option: a free-text row → enter input mode; anything else confirms.
    pub(crate) fn ask_click(&mut self, index: usize) {
        let Some((request, _)) = &self.pending_ask else {
            return;
        };
        let options_len = request.options.len();
        let free_text = request.free_text;
        if index >= options_len && free_text {
            self.ask_focus = index;
            return;
        }
        self.choose_ask_option(index);
    }

    /// AskUserQuestion answer text: header + one `· question → answer` line.
    /// Enters the message flow as an ordinary user message (no longer a
    /// transient block rendered above the input).
    fn ask_answer_text(question: &str, answer: &str) -> String {
        format!("User answered the questions:\n  · {question} → {answer}")
    }

    /// Records an answer/decline/receipt as an ordinary user message: rendered like user
    /// input, settled and flushed into scrollback, persistent with the session — no transient
    /// residue. The turn goes on after an answer, so a fresh assistant message opens behind it;
    /// everything the model does next then reads below what the user just said.
    fn push_ask_message(&mut self, text: String) {
        self.push_user_line(text);
        self.open_continuation_message();
    }

    /// A user-role line the user never wrote (`is_state_line`): the receipt a
    /// resolved dialog leaves where the dialog was, or the line a cancelled one
    /// leaves. It renders as one dim line and carries no send stamp, because
    /// nothing was sent — and it never reaches the model, which learns the
    /// verdict from the gate instead.
    pub(super) fn push_user_line(&mut self, text: String) {
        self.messages.push(UiMessage {
            role: Role::User,
            text,
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
            digest: false,
        });
    }

    /// Submitting free text. Empty is not an answer: AskUserQuestion reads it as
    /// a decline, a permission prompt as the plain refusal it already is.
    fn submit_ask_answer(&mut self, text: String) {
        let permission = self
            .pending_ask
            .as_ref()
            .is_some_and(|(request, _)| request.kind == AskKind::Permission);
        if text.trim().is_empty() {
            if permission {
                self.push_ask_message(ASK_RECEIPT_NO.to_string());
            } else if self
                .pending_ask
                .as_ref()
                .is_some_and(|(request, _)| request.free_text)
            {
                self.push_ask_message(ASK_DECLINED_TEXT.to_string());
            }
            if let Some((_, tx)) = self.pending_ask.take() {
                let _ = tx.send(DialogAction::Cancel);
            }
            self.reset_ask_state();
            return;
        }
        if let Some((request, tx)) = self.pending_ask.take() {
            if request.kind == AskKind::Permission {
                self.push_ask_message(format!("{}{}", ASK_RECEIPT_NO_PREFIX, text.trim()));
            } else {
                let question = request.question.clone();
                self.push_ask_message(Self::ask_answer_text(&question, &text));
            }
            let _ = tx.send(DialogAction::Answer(text));
        }
        self.reset_ask_state();
    }

    /// Confirms option `index` (0-based; out of range = cancel).
    pub(crate) fn choose_ask_option(&mut self, index: usize) {
        let Some((request, _)) = &self.pending_ask else {
            return;
        };
        // The refusal option does not resolve the dialog: the user said no and
        // is about to say what to do instead. Empty submit or Esc from there is
        // still the plain refusal.
        if request.refusal_option() == Some(index) && !request.free_text {
            let options_len = request.options.len();
            if let Some((request, _)) = &mut self.pending_ask {
                request.free_text = true;
            }
            self.ask_focus = options_len;
            self.ask_other.clear();
            self.dirty = true;
            return;
        }
        let receipt = request.kind.eq(&AskKind::Permission).then(|| {
            if request.session_option() == Some(index) {
                ASK_RECEIPT_SESSION
            } else {
                ASK_RECEIPT_YES
            }
        });
        if let Some((request, tx)) = self.pending_ask.take() {
            if index < request.options.len() {
                if let Some(receipt) = receipt {
                    self.push_ask_message(receipt.to_string());
                } else if request.free_text {
                    let question = request.question.clone();
                    let answer = request.options[index].clone();
                    self.push_ask_message(Self::ask_answer_text(&question, &answer));
                }
                let _ = tx.send(DialogAction::Confirm(index));
            } else {
                let _ = tx.send(DialogAction::Cancel);
            }
        }
        self.reset_ask_state();
    }

    /// Permission/ask block (PermissionDialog / AskUserQuestion):
    /// title (permission bold) + description (dim) + the pre-approval preview +
    /// numbered options (Select: `❯ n. label` focus marker, desc sub-row dim,
    /// free-text input) + shortcut hints.
    pub(crate) fn ask_el(&self, theme: &Theme) -> Option<El> {
        let (request, _) = self.pending_ask.as_ref()?;
        let permission = request.kind == AskKind::Permission;
        let mut parts: Vec<El> = Vec::new();
        let mut title = Line::styled("⏺ ", SegStyle::fg(theme.text));
        title.push_styled(request.title.clone(), theme.permission());
        parts.push(El::Line(title));
        parts.push(El::Line(Line::styled(
            format!("  {}", request.question),
            SegStyle::fg(theme.text),
        )));
        let mut expandable = false;
        if let Some(preview) = &request.preview {
            let (rows, hidden) = self.preview_rows(preview, theme);
            // ctrl+e is advertised whenever it would reveal something: withheld
            // preview rows, or the rule behind the session option.
            expandable = hidden > 0 || request.scope.is_some() || self.ask_expanded;
            parts.push(El::Blank);
            parts.extend(rows.into_iter().map(El::Line));
            if hidden > 0 {
                parts.push(El::Line(Line::styled(
                    format!("  … {hidden} more lines"),
                    theme.dim(),
                )));
            }
            // Expanded is also where the rule behind option 2 is spelled out:
            // "this session" is a promise, and this is its exact wording.
            if self.ask_expanded
                && let Some(scope) = &request.scope
            {
                parts.push(El::Line(Line::styled(
                    format!("  session rule: {scope}"),
                    theme.dim(),
                )));
            }
        }
        // CC Select: one blank row between the question and the options.
        parts.push(El::Blank);
        let focus_color = theme.permission;
        let feedback_open = permission && request.free_text;
        for (opt_idx, option) in request.options.iter().enumerate() {
            let focused = opt_idx == self.ask_focus
                || (feedback_open && opt_idx + 1 == request.options.len());
            let mut line = Line::empty();
            let style = if focused {
                SegStyle::fg(focus_color)
            } else {
                SegStyle::fg(theme.text_secondary)
            };
            line.push_styled(if focused { "❯ " } else { "  " }, style);
            line.push_styled(format!("{}. {option}", opt_idx + 1), style);
            // Only the option row itself confirms; the description sub-row stays inert.
            parts.push(El::click(ClickTarget::AskOption(opt_idx), El::Line(line)));
            if let Some(desc) = request
                .descriptions
                .get(opt_idx)
                .and_then(|d| d.as_deref())
                .filter(|d| !d.is_empty())
            {
                parts.push(El::Line(Line::styled(
                    format!("   {desc}"),
                    if focused {
                        SegStyle::fg(focus_color)
                    } else {
                        SegStyle::fg(theme.text_secondary)
                    },
                )));
            }
            // The feedback row hangs off the refusal option rather than taking a
            // number of its own: it is that option being answered, not a fourth
            // thing to choose.
            if feedback_open && opt_idx + 1 == request.options.len() {
                parts.push(El::Line(Line::styled(
                    format!(
                        "   {}",
                        self.free_text_row("Tell bingo what to do instead.")
                    ),
                    SegStyle::fg(focus_color),
                )));
            }
        }
        if request.free_text && !permission {
            let other_idx = request.options.len();
            let focused = self.ask_focus >= other_idx;
            let mut line = Line::empty();
            let style = if focused {
                SegStyle::fg(focus_color)
            } else {
                SegStyle::fg(theme.text_secondary)
            };
            line.push_styled(if focused { "❯ " } else { "  " }, style);
            line.push_styled(format!("{}. Other", other_idx + 1), style);
            parts.push(El::click(ClickTarget::AskOption(other_idx), El::Line(line)));
            // No cursor glyph: the terminal cursor is the only caret in the app, and it stays
            // anchored to the input box below (the ask block renders into the transcript).
            parts.push(El::Line(Line::styled(
                format!("   {}", self.free_text_row("Type something.")),
                if focused {
                    SegStyle::fg(focus_color)
                } else {
                    SegStyle::fg(theme.text_secondary)
                },
            )));
        }
        parts.push(El::Line(Line::styled(
            format!("  {}", self.ask_hint(request, expandable)),
            theme.muted(),
        )));
        Some(El::Col(parts))
    }

    /// What the free-text row shows: what has been typed, or its placeholder.
    fn free_text_row(&self, placeholder: &str) -> String {
        if self.ask_other.is_empty() {
            placeholder.to_string()
        } else {
            self.ask_other.clone()
        }
    }

    fn ask_hint(&self, request: &crate::ui::PermissionRequest, expandable: bool) -> String {
        // A permission prompt has no cancel: leaving it *is* the refusal, and
        // the hint says so rather than suggesting the question goes away.
        let permission = request.kind == AskKind::Permission;
        let leave = if permission {
            "esc to deny"
        } else {
            "esc to cancel"
        };
        let mut hint = if request.free_text && self.ask_focus >= request.options.len() {
            vec!["enter to submit", leave]
        } else {
            vec!["enter to select", "↑/↓ to navigate", leave]
        };
        if expandable {
            let expand = if self.ask_expanded {
                "ctrl+e to collapse"
            } else {
                "ctrl+e to expand"
            };
            hint.insert(hint.len() - 1, expand);
        }
        hint.join(" · ")
    }

    /// Preview rows plus the number withheld by the collapsed bound.
    fn preview_rows(&self, preview: &AskPreview, theme: &Theme) -> (Vec<Line>, usize) {
        let (lines, cap) = match preview {
            AskPreview::Command(command) => (
                command
                    .lines()
                    .map(|line| {
                        let mut row = Line::styled("  $ ", theme.dim());
                        row.push_styled(line.to_string(), SegStyle::fg(theme.text));
                        row
                    })
                    .collect::<Vec<_>>(),
                ASK_COMMAND_ROWS,
            ),
            AskPreview::Diff(text) => (
                // The dialog indents every preview row by two columns, so the
                // diff gets the width that is actually left for it.
                diff_lines(
                    &Diff::parse_unified(text),
                    theme,
                    self.width.saturating_sub(2),
                )
                .into_iter()
                .map(|mut line| {
                    line.prepend_styled("  ", theme.dim());
                    line
                })
                .collect::<Vec<_>>(),
                ASK_DIFF_ROWS,
            ),
        };
        if self.ask_expanded || lines.len() <= cap {
            return (lines, 0);
        }
        let hidden = lines.len() - cap;
        (lines.into_iter().take(cap).collect(), hidden)
    }
}
