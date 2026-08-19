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

/// Bounded preview height, collapsed. A long diff would push the question and
/// the options themselves off the screen — ctrl+e lifts the bound.
const ASK_DIFF_ROWS: usize = 12;
/// Bounded command height, collapsed (heredocs and `&&` chains run long).
const ASK_COMMAND_ROWS: usize = 6;

use crate::app::snapshot::{ActivationKind, InteractionDecision};

impl super::Chat {
    /// Picks up the prompt the core has open (one at a time: a second question
    /// waits until this one is settled).
    ///
    /// The prompt is the actor's; this reads the projection the core's
    /// `interaction/opened` frame folded into the store, which is where every
    /// client learns about a prompt.
    pub fn drain_asks(&mut self) -> bool {
        // A prompt this console adopted and then settled comes off the screen
        // when the core says it is closed — and is not adopted again in the
        // meantime. Acting is a write and being told is a frame, so between the
        // two the projection still holds a prompt that is already answered;
        // `last_ask` is how the client remembers which one that is.
        if let Some((id, _)) = &self.pending_ask
            && self.last_ask.as_ref() == Some(id)
            && self.store.view().interaction(id).is_none()
        {
            self.pending_ask = None;
            self.reset_ask_state();
            self.dirty = true;
        }
        if self.pending_ask.is_some() {
            return false;
        }
        let settled = self.last_ask.clone();
        let Some(open) = self
            .store
            .view()
            .open_interactions()
            .find(|open| Some(&open.id) != settled.as_ref())
        else {
            return false;
        };
        let pending = (open.id.clone(), crate::ui::PermissionRequest::of(open));
        self.ask_focus = 0;
        self.ask_other.clear();
        self.ask_expanded = false;
        self.last_ask = Some(pending.0.clone());
        self.pending_ask = Some(pending);
        // The turn is blocked until this is answered, and the user may well
        // be looking somewhere else by now (D79).
        self.notify.attention(Attention::WaitingPermission);
        self.notify.set_title(Title::WaitingPermission);
        true
    }

    /// Settles the permission dialogs a dead turn left behind (D80).
    ///
    /// The dialog and the turn used to have separate lifetimes: an interrupt
    /// killed the task awaiting the answer and left the question on screen, so
    /// the footer went on saying `Waiting for permission…` and every 1-9 the
    /// user pressed answered a corpse. Cancelling closes both ends — the core
    /// fails the prompt closed, and one dim line goes in the flow where the
    /// dialog was.
    ///
    /// `dead_only` keeps a background agent out of the foreground turn's
    /// cleanup: subagents share this modal, so at turn end the only prompts that
    /// belong to the turn that just ended are the ones whose run is already gone.
    /// An explicit interrupt takes everything, because that is what the user
    /// asked for.
    pub(crate) fn cancel_asks(&mut self, dead_only: bool) {
        self.intend(crate::tui::intent::Intent::CancelAsks { dead_only });
    }

    /// What the console does about prompts the core closed.
    pub(crate) fn asks_cancelled(&mut self) {
        // What is on screen may have been one of them; the next drain picks
        // up whatever is still open.
        self.pending_ask = None;
        self.reset_ask_state();
        self.main_conv().drop_empty_stream_message();
        self.push_user_line(ASK_CANCELLED_TEXT.to_string());
        // The title still announced a question nobody could answer.
        self.notify_idle();
        self.dirty = true;
    }

    fn reset_ask_state(&mut self) {
        self.ask_focus = 0;
        self.ask_other.clear();
        self.ask_expanded = false;
    }

    /// The prompt on screen, if there is one.
    fn ask_request(&self) -> Option<&crate::ui::PermissionRequest> {
        self.pending_ask.as_ref().map(|(_, request)| request)
    }

    /// Answer the prompt on screen, with the receipt the answer earns.
    ///
    /// The receipt travels with the answer rather than being written beside it,
    /// because only an answer the core *took* has earned one: D81's guard
    /// refuses one that came too fast, and the dialog stays exactly where it was
    /// (D154 moved the waiting off the key handler; the refusal is unchanged).
    fn answer(
        &mut self,
        activation: ActivationKind,
        at: std::time::Instant,
        decision: InteractionDecision,
        receipt: Option<String>,
    ) -> bool {
        let Some((id, _)) = &self.pending_ask else {
            return false;
        };
        self.intend(crate::tui::intent::Intent::AnswerAsk {
            id: id.clone(),
            activation,
            at,
            decision,
            receipt,
        });
        true
    }

    /// What the console does about an answer the core took.
    pub(crate) fn ask_answered(&mut self, receipt: Option<String>) {
        self.pending_ask = None;
        self.reset_ask_state();
        if let Some(line) = receipt {
            self.push_ask_message(line);
        }
    }

    /// The decision option `index` stands for on the prompt in hand.
    fn decision_at(&self, index: usize) -> Option<InteractionDecision> {
        let (id, request) = self.pending_ask.as_ref()?;
        let _ = id;
        if request.kind != AskKind::Permission {
            return Some(InteractionDecision::Answer {
                option_id: Some(index.to_string()),
                text: None,
            });
        }
        if request.session_option() == Some(index) {
            let scope = self.open_scope()?;
            return Some(InteractionDecision::AllowSession { scope_id: scope });
        }
        if index == 0 {
            return Some(InteractionDecision::AllowOnce);
        }
        Some(InteractionDecision::Deny { feedback: None })
    }

    /// The scope identifier the core minted for this prompt's session rule.
    fn open_scope(&self) -> Option<crate::app::ids::ScopeId> {
        let (id, _) = self.pending_ask.as_ref()?;
        match &self.store.view().interaction(id)?.prompt {
            crate::app::snapshot::InteractionPrompt::Permission { session_scope, .. } => {
                session_scope.as_ref().map(|scope| scope.id.clone())
            }
            _ => None,
        }
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
        let Some(request) = self.ask_request() else {
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
                self.submit_ask_answer(text, now);
                true
            }
            // shift+tab is CC's shortcut onto the session option, from anywhere
            // in the dialog. Without one offered it does nothing: the dialog
            // owns the key while it is open, and cycling the permission mode
            // behind an unanswered question is not what the press meant.
            KeyCode::BackTab if permission => {
                if let Some(index) = self.session_option() {
                    self.choose_ask_option_at(index, ActivationKind::Keyboard, now);
                }
                true
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let index = (c as u8 - b'1') as usize;
                if index < total {
                    self.ask_focus = index;
                    if !(index == options_len && free_text) {
                        // The core is what refuses a premature approval; the key
                        // is swallowed either way, exactly as it always was.
                        self.choose_ask_option_at(index, ActivationKind::Keyboard, now);
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
                let focus = self.ask_focus;
                if focus >= options_len && free_text {
                    let text = std::mem::take(&mut self.ask_other);
                    self.submit_ask_answer(text, now);
                } else {
                    self.choose_ask_option_at(focus, ActivationKind::Keyboard, now);
                }
                true
            }
            // Esc denies plainly, from the option list and from the feedback row
            // alike: the user is leaving, not composing. Dismissing is never
            // premature, so it is never held back.
            KeyCode::Esc => {
                let Some(request) = self.ask_request() else {
                    return true;
                };
                let receipt = Self::refusal_receipt(request);
                self.answer(
                    ActivationKind::Keyboard,
                    now,
                    InteractionDecision::Cancel,
                    receipt,
                );
                true
            }
            _ => false,
        }
    }

    /// Index of the "don't ask again this session" option, when it is offered.
    fn session_option(&self) -> Option<usize> {
        self.ask_request()
            .and_then(|request| request.session_option())
    }

    /// Click on a dialog option: a free-text row → enter input mode; anything else confirms.
    ///
    /// A pointer was aimed at the prompt that exists, so it is never premature.
    pub(crate) fn ask_click(&mut self, index: usize) {
        let Some(request) = self.ask_request() else {
            return;
        };
        let options_len = request.options.len();
        let free_text = request.free_text;
        if index >= options_len && free_text {
            self.ask_focus = index;
            return;
        }
        self.choose_ask_option_at(index, ActivationKind::Pointer, std::time::Instant::now());
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
        self.main_conv().messages.push(UiMessage {
            speaker: None,
            role: Role::User,
            text,
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
    }

    /// Submitting free text. Empty is not an answer: AskUserQuestion reads it as
    /// a decline, a permission prompt as the plain refusal it already is.
    fn submit_ask_answer(&mut self, text: String, now: std::time::Instant) {
        let Some(request) = self.ask_request() else {
            return;
        };
        let permission = request.kind == AskKind::Permission;
        let question = request.question.clone();
        if text.trim().is_empty() {
            let receipt = Self::refusal_receipt(request);
            self.answer(
                ActivationKind::Keyboard,
                now,
                InteractionDecision::Cancel,
                receipt,
            );
            return;
        }
        let decision = if permission {
            // The refusal carries a direction: it travels to the model inside
            // the `<permission_error>`.
            InteractionDecision::Deny {
                feedback: Some(text.clone()),
            }
        } else {
            InteractionDecision::Answer {
                option_id: None,
                text: Some(text.clone()),
            }
        };
        let receipt = Some(if permission {
            format!("{}{}", ASK_RECEIPT_NO_PREFIX, text.trim())
        } else {
            Self::ask_answer_text(&question, &text)
        });
        self.answer(ActivationKind::Keyboard, now, decision, receipt);
    }

    /// The line a plain refusal leaves, if it leaves one.
    fn refusal_receipt(request: &crate::ui::PermissionRequest) -> Option<String> {
        if request.kind == AskKind::Permission {
            Some(ASK_RECEIPT_NO.to_string())
        } else if request.free_text {
            Some(ASK_DECLINED_TEXT.to_string())
        } else {
            None
        }
    }

    /// Confirms option `index`, as `activation` at `at`. Returns whether the
    /// core took the answer.
    fn choose_ask_option_at(
        &mut self,
        index: usize,
        activation: ActivationKind,
        at: std::time::Instant,
    ) -> bool {
        let Some(request) = self.ask_request() else {
            return false;
        };
        // The refusal option does not resolve the dialog: the user said no and
        // is about to say what to do instead. Empty submit or Esc from there is
        // still the plain refusal.
        if request.refusal_option() == Some(index) && !request.free_text {
            let options_len = request.options.len();
            if let Some((_, request)) = &mut self.pending_ask {
                request.free_text = true;
            }
            self.ask_focus = options_len;
            self.ask_other.clear();
            self.dirty = true;
            return true;
        }
        if index >= request.options.len() {
            let receipt = Self::refusal_receipt(request);
            return self.answer(activation, at, InteractionDecision::Cancel, receipt);
        }
        let permission = request.kind == AskKind::Permission;
        let free_text = request.free_text;
        let question = request.question.clone();
        let label = request.options[index].clone();
        let session = request.session_option() == Some(index);
        let receipt = if permission {
            Some(
                if session {
                    ASK_RECEIPT_SESSION
                } else {
                    ASK_RECEIPT_YES
                }
                .to_string(),
            )
        } else if free_text {
            Some(Self::ask_answer_text(&question, &label))
        } else {
            None
        };
        let Some(decision) = self.decision_at(index) else {
            return false;
        };
        self.answer(activation, at, decision, receipt)
    }

    /// Permission/ask block (PermissionDialog / AskUserQuestion):
    /// title (permission bold) + description (dim) + the pre-approval preview +
    /// numbered options (Select: `❯ n. label` focus marker, desc sub-row dim,
    /// free-text input) + shortcut hints.
    pub(crate) fn ask_el(&self, theme: &Theme) -> Option<El> {
        let (_, request) = self.pending_ask.as_ref()?;
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
