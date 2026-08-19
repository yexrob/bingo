//! Tail half of [`Chat`]'s methods (split out of chat.rs, #8):
//! provider/think/skills menus, key handling, turn submission and the
//! input/editing surface. Owns no state; `impl super::Chat`.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::query::Session;
use crate::tui::composer::KillDir;

/// Everything Esc can dismiss, in the order it dismisses them (D80). The key
/// handler walks [`EscLayer::ORDER`] top-down, acts on the first layer that is
/// open and stops: one press closes one level.
///
/// The busy interrupt sits *inside* the list rather than above it, and that
/// placement is the whole point. A dropdown, an info block or the `?` panel
/// opened over a running turn is what the user is looking at when they reach
/// for Esc; closing the turn instead was answering a question nobody asked.
/// Ctrl+C keeps the unconditional interrupt — the layers do not shield it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscLayer {
    /// Permission / question dialog: Esc denies it.
    AskDialog,
    /// `/model` `/think` `/theme` `/resume` `/provider` pickers (one level per press).
    Menu,
    /// The esc-esc rewind selector (D91): the action list returns to the turn
    /// list, the turn list closes.
    ///
    /// Menu-tier, beside the pickers rather than above them: it is a transient
    /// chooser over the composer, and it opens and closes on its own key.
    Rewind,
    /// The `ctrl+b` background dialog (D107): agents, shells and rooms.
    ///
    /// **One layer, not two**, though it has a list and a detail: CC's detail
    /// *replaces* the list rather than stacking on it
    /// (`BackgroundTasksDialog.tsx:396`), and its own footer says
    /// `Esc/Enter/Space to close` from either mode
    /// (`InProcessTeammateDetailDialog.tsx:198`). `←` is the way back to the
    /// list, so nothing is stranded by a press that closes the modal.
    BackgroundDialog,
    /// Slash-command dropdown: closes, and takes a bare `/` query with it.
    SlashDropdown,
    /// The `@` mention dropdown: closes, leaving the typed token alone.
    /// Same stratum as [`EscLayer::SlashDropdown`] — both are transient
    /// completion surfaces over the composer, and the two are mutually
    /// exclusive, so their relative order is a formality.
    MentionDropdown,
    /// ctrl+r history search: cancels without adopting the hit.
    Search,
    /// A page/field-level error row above the prompt.
    ErrorRow,
    /// Info-tier lines (`/help` `/status` … output that persists until dismissed).
    InfoLines,
    /// The `?` shortcut panel.
    HelpPanel,
    /// The roster's cursor (v6). The rows themselves are constant furniture
    /// under the composer — what Esc takes is the selection on them, exactly
    /// CC's escape in `selecting-agent` (`useBackgroundTaskNavigation.ts:
    /// 166-175`): the list stays, the cursor leaves.
    Roster,
    /// The task panel, when the user opened it themselves with ctrl+t.
    TaskPanel,
    /// The away page's running turn (v6): Esc stops the agent on screen, the
    /// way it interrupts main's turn at home — and only that agent's; main's
    /// own turn is out of reach until the page comes home.
    AwayStop,
    /// The running turn.
    Interrupt,
    /// Bash mode on an empty input. Below the interrupt, unlike every other
    /// layer, because the `!` prefix is sticky: a running bash command always
    /// sits under an empty bash-mode composer, and Esc there has to reach the
    /// command rather than the prompt prefix.
    BashMode,
    /// A non-empty input: esc-esc clears it into history.
    ClearInput,
    /// The away page itself (v6): the last thing Esc peels is the page,
    /// which is CC's `exitTeammateView` — everything above it got its press
    /// first, so leaving is deliberate.
    AwayHome,
}

impl EscLayer {
    /// The stack, top first. The single source for Esc's priority.
    pub const ORDER: [EscLayer; 17] = [
        EscLayer::AskDialog,
        EscLayer::Menu,
        EscLayer::Rewind,
        EscLayer::BackgroundDialog,
        EscLayer::SlashDropdown,
        EscLayer::MentionDropdown,
        EscLayer::Search,
        EscLayer::ErrorRow,
        EscLayer::InfoLines,
        EscLayer::HelpPanel,
        EscLayer::Roster,
        EscLayer::TaskPanel,
        EscLayer::AwayStop,
        EscLayer::Interrupt,
        EscLayer::BashMode,
        EscLayer::ClearInput,
        EscLayer::AwayHome,
    ];
}

/// Who a flow row belongs to, for the gutter face and for sender grouping.
///
/// The transcript has exactly two speakers and they are never written down
/// anywhere — the role *is* the name — so they are named here, and since D99
/// the console wears the same gutter every other view wears.
///
/// **A state line belongs to nobody** (D99 review). The console's user-role
/// rows are not all the user's: a failed-agent alert, an ask receipt, an
/// interrupt marker and a rewind line are the runtime reporting, and the
/// human's portrait beside `⚠ @scout · connection reset` would say the human
/// wrote it. Returning `None` costs them the face and takes them out of the
/// run, so main speaking → alert → main speaking again re-leads with main's
/// face; the gutter *indentation* is not decided here, and they keep it, so the
/// message column never jogs.
///
/// A steered message (`↪ …`) is not a state line and is not covered: the user
/// typed it, so the face is right.
fn speaker_of(role: Role, text: &str) -> Option<String> {
    match role {
        Role::Assistant => Some(crate::channels::MAIN_NAME.to_string()),
        Role::User if crate::tui::chat::is_state_line(text) => None,
        Role::User => Some(crate::channels::USER_NAME.to_string()),
    }
}

/// Rows a window must keep free of dispatch progress before the rows condense
/// into one line — CC's `TERMINAL_BUFFER_LINES` (`tools/AgentTool/UI.tsx:182`),
/// verbatim.
pub(crate) const DISPATCH_BUFFER_LINES: usize = 7;

impl super::Chat {
    pub(crate) fn slash_skills(&mut self) {
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let skills = crate::skills::load_skills(&home, &cwd);
        if skills.is_empty() {
            self.push_slash_output(
                "no skills available.\nSkills live in .bingo/skills/<name>/SKILL.md or $XDG_CONFIG_HOME/bingo/skills/<name>/SKILL.md."
                    .to_string(),
            );
            return;
        }
        let listing = crate::skills::format_listing(&skills, crate::skills::DEFAULT_CHAR_BUDGET);
        self.push_slash_info(format!("available skills:\n{listing}"));
    }

    pub(crate) fn slash_tasks(&mut self) {
        self.refresh_tasks();
        // task_lines is gated by task-area visibility — /tasks explicitly asks for them, so bypass it temporarily.
        let was_visible = self.tasks_visible;
        self.tasks_visible = true;
        let lines = self.task_lines();
        self.tasks_visible = was_visible;
        if lines.is_empty() {
            self.push_slash_output("no background tasks right now.".to_string());
            return;
        }
        let text: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        self.push_slash_info(text.join("\n"));
    }

    /// `/team <subcommand>` (D31 project-level formation): dispatched to
    /// team_cmd, and the answer lands on the info tier — the tier every other
    /// slash command answers on.
    ///
    /// D90 filed it in the team feed instead, because the board was the
    /// formation's own history; D95 made that feed a column of the team
    /// directory, D104 took the directory's door away and printed here as
    /// well, and D107 retired the column with the directory. A store nobody
    /// reads is not a store, so the tier is now the whole answer.
    pub(crate) fn slash_team_read(&mut self, what: crate::app::action::TeamRead) {
        let lines =
            crate::team_cmd::read(&self.session, &std::path::PathBuf::from(&self.cwd), what);
        for line in lines {
            self.push_slash_info(line);
        }
        self.dirty = true;
    }

    /// The other half: what `/team` was asked to *do*, already read.
    pub(crate) fn slash_team_act(&mut self, action: &crate::app::command::Action) {
        let said = crate::engine::actions::team(&self.session, action);
        self.push_slash_info(said.text);
        self.dirty = true;
    }

    /// Rebuilds the composer's dropdown after an edit. One surface at a time,
    /// in the order the caret decides: an `@` token under the caret (D85), else
    /// the argument phase of a slash line (D85), else the command-name phase.
    pub(crate) fn update_slash_suggestions(&mut self) {
        self.clear_slash_suggestions();
        self.update_mention();
        if self.mention.is_some() {
            return;
        }
        if let Some(ctx) = crate::tui::complete::arg_context(&self.input) {
            // Past the command name, whatever happens next: a free-form
            // argument offers nothing rather than falling back to re-offering
            // the command the user has already finished typing.
            if let Some(candidates) = self.arg_candidates(&ctx) {
                let start = ctx.start;
                let items =
                    crate::tui::complete::fuzzy_rank(ctx.partial, candidates, |candidate| {
                        candidate.value.as_str()
                    });
                self.slash_arg_start = Some(start);
                self.slash_suggestions = items
                    .into_iter()
                    .map(|candidate| SlashSuggestion {
                        name: candidate.value,
                        hint: String::new(),
                        description: candidate.description,
                    })
                    .collect();
                self.slash_selected = self
                    .slash_selected
                    .min(self.slash_suggestions.len().saturating_sub(1));
            }
            // The name-phase "no matching commands" hint would be a lie here:
            // an unmatched argument is still a legal thing to type.
            self.slash_no_match = false;
            return;
        }
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let skills = crate::skills::load_skills(&home, &cwd)
            .into_iter()
            .map(|skill| {
                let mut description = skill.description;
                if description.chars().count() > crate::skills::MAX_LISTING_DESC_CHARS {
                    let cut: String = description
                        .chars()
                        .take(crate::skills::MAX_LISTING_DESC_CHARS - 1)
                        .collect();
                    description = format!("{cut}…");
                }
                SlashSuggestion {
                    name: skill.name,
                    hint: String::new(),
                    description,
                }
            });
        let result = crate::tui::slash::suggestions(
            &self.input,
            &crate::tui::slash::commands(),
            skills,
            // Full list: rendering windows around the selection (the old
            // hard cap made commands 6+ unreachable from a bare `/`).
            usize::MAX,
        );
        self.slash_suggestions = result.items;
        self.slash_selected = self
            .slash_selected
            .min(self.slash_suggestions.len().saturating_sub(1));
        self.slash_no_match = result.no_match;
    }

    /// Dropdown key handling: ↑↓ move the selection, Tab completes (without running), Esc closes.
    /// No j/k navigation: while the menu is open, j/k would be typed as input chars (e.g. /thin → think),
    /// swallowing keys and truncating the command. Returns true = consumed.
    pub(crate) fn slash_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.slash_suggestions.is_empty() {
            return false;
        }
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash_selected = (self.slash_selected + 1) % self.slash_suggestions.len();
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash_selected = self
                    .slash_selected
                    .checked_sub(1)
                    .unwrap_or(self.slash_suggestions.len() - 1);
                true
            }
            KeyCode::Tab => {
                self.apply_slash_suggestion();
                true
            }
            KeyCode::Esc => {
                // The argument phase keeps its line: `/model dee` is a command
                // the user is halfway through typing, not a stray query.
                let name_phase = self.slash_arg_start.is_none();
                self.clear_slash_suggestions();
                // The name dropdown only exists for a pure `/`-query — dismissing
                // it dismisses the query too (the leftover "/th" used to turn the
                // next command into "//model").
                if name_phase && self.input.starts_with('/') {
                    self.set_input("");
                }
                true
            }
            _ => false,
        }
    }

    /// Applies the selected suggestion (applyCommandSuggestion). In the name
    /// phase that is `/name ` for the whole line; in the argument phase (D85)
    /// the value is spliced over the partial token, leaving the command and any
    /// earlier arguments alone. Both end with a space, so the next argument's
    /// dropdown opens straight away.
    fn apply_slash_suggestion(&mut self) {
        if let Some(s) = self.slash_suggestions.get(self.slash_selected) {
            match self.slash_arg_start {
                Some(start) if start <= self.input.len() && self.input.is_char_boundary(start) => {
                    self.input = format!("{}{} ", &self.input[..start], s.name);
                }
                _ => self.input = format!("/{} ", s.name),
            }
            // The caret follows the text it just completed; it used to stay
            // where the partial word ended, leaving it mid-line.
            self.cursor = self.input.len();
        }
        self.clear_slash_suggestions();
    }

    /// The tool barrier, taken by hand: the same atomic absorb the running turn's
    /// steering source performs, for a test that has no provider behind it.
    #[cfg(test)]
    pub(crate) fn take_steering(&self) -> Vec<crate::app::queue::SteerItem> {
        self.session
            .queue
            .absorb(
                crate::ui::ConvKey::Main,
                crate::app::ids::TurnId::new("turn_test"),
            )
            .now()
    }

    /// Main's queue as it stands.
    pub(crate) fn main_queue(&self) -> crate::app::queue::ConversationQueue {
        self.session.queue.of(&crate::ui::ConvKey::Main)
    }

    /// The queue rows of the page on screen. Only the console has one; every other
    /// page reads an empty queue rather than main's.
    pub(crate) fn page_queue(&self) -> crate::app::queue::ConversationQueue {
        self.session.queue.of(&self.active)
    }

    /// The running turn took these queued messages into its own context at a tool
    /// barrier: they are in the request already, and the core has already taken them
    /// out of the queue. What is left is where they land on screen.
    ///
    /// The reply block is split there. One turn renders as one assistant message, so a
    /// line merely pushed after it would sink below everything the turn still had to
    /// say; closing the block and opening a continuation — the same move an
    /// AskUserQuestion answer makes — puts the message between the reply written
    /// without it and the reply written with it, which is the order the history holds.
    pub(crate) fn absorb_steered(&mut self, items: &[crate::app::queue::SteerItem]) {
        if items.is_empty() {
            return;
        }
        for item in items {
            self.push_steered_line(&item.text);
        }
        self.open_continuation_message();
        self.dirty = true;
    }

    /// The transcript line a steered message leaves: the user's own words under the
    /// `↪` marker, rendered as a single dim line rather than a `❯` bubble.
    fn push_steered_line(&mut self, text: &str) {
        self.main_conv().messages.push(UiMessage {
            speaker: None,
            role: Role::User,
            text: format!("{}{text}", crate::app::queue::STEER_FLOW_PREFIX),
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
    }

    /// The alert line a failed run leaves in the flow (D98), plus the D79 ring.
    ///
    /// The label is the run's, and every shape of it — `scout · fix the parser`,
    /// `scout #3 · look again`, `scout #7 receipt` — opens with the instance
    /// name, which is the only part of it the user needs here.
    pub(crate) fn push_agent_alert(&mut self, label: &str, reason: Option<&str>) {
        let instance = label.split_whitespace().next().unwrap_or(label);
        let line = crate::tui::bufferview::agent_alert_line(instance, reason);
        self.main_conv().messages.push(UiMessage {
            speaker: None,
            role: Role::User,
            text: line,
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        // @main's own badge (D99). An alert is the one line here nobody chose to
        // say, so it is the one that earns the accent: a reader in another
        // conversation must be able to tell "main answered" from "something
        // broke" without coming back to look.
        self.buffers.note_console(true, self.tick);
        self.notify
            .attention(crate::tui::notify::Attention::AgentNotice);
        self.dirty = true;
    }

    /// One dim line for the task notification a finished run just put in main's
    /// context (D106), before main says anything about it.
    ///
    /// No bell and no badge: the run ended the way it was supposed to, and the
    /// dispatch row two lines up already settled into `Done (…)`. The alert
    /// above is what bad news costs; this is what good news costs.
    pub(crate) fn push_agent_notice(&mut self, label: &str) {
        self.push_flow_line(crate::tui::bufferview::agent_notice_line(label));
    }

    /// A user-role line the harness wrote about somebody else's life.
    fn push_flow_line(&mut self, text: String) {
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
        self.dirty = true;
    }

    /// The main agent's inbox, digested on a quiet window rather than per message
    /// (D98).
    ///
    /// **Neither the window nor the turn is decided here** (乙案, B4; D154).
    /// Coalescing, the deadline, urgency, the idle gates *and* opening the turn
    /// are `app::mail`'s and the core's one answer, so a GUI wakes main exactly
    /// as this does rather than reimplementing a debounce that could drift from
    /// it. What is left here is the console's own half: ringing its attention
    /// channel.
    ///
    /// Returns whether a digest turn was opened this frame.
    pub(crate) fn digest_mail(&mut self) {
        self.intend_once(crate::tui::intent::Intent::Digest);
    }

    /// Open main's turn in the core, for a test that wants one running without a
    /// submission behind it.
    #[cfg(test)]
    pub(crate) fn open_core_turn(
        &self,
        origin: crate::app::snapshot::TurnOrigin,
    ) -> Option<crate::app::ids::TurnId> {
        self.session
            .turns
            .open(crate::ui::ConvKey::Main, origin, Vec::new())
            .now()
    }

    /// Turn-level error message with auth guidance for the current provider:
    /// `AUTH_REQUIRED` on an oauth-configured provider appends a re-login
    /// hint (the raw API error body rarely tells the user what to do);
    /// `PERMISSION_DENIED` points at the model/subscription (D33 §6.4).
    fn auth_error_hint(session: &Session, code: &str, msg: String) -> String {
        let provider = session.core.config().borrow().provider.clone();
        // Merge built-in presets: zero-config codex (no settings entry) is the
        // main preset use case — without this, its expired token produced a
        // bare 401 with no re-login guidance.
        let oauth = session
            .settings
            .providers
            .get(&provider)
            .map(|c| c.oauth.is_some())
            .or_else(|| {
                crate::api::providers::presets::preset(&provider).map(|p| p.oauth_kind.is_some())
            })
            .unwrap_or(false);
        auth_hint_for(oauth, &provider, code, msg)
    }

    /// The turn-level error message this session would show for a code, with the
    /// provider guidance the raw body never carries.
    pub(crate) fn auth_hint(&self, code: &str, msg: String) -> String {
        Self::auth_error_hint(&self.session, code, msg)
    }

    /// The answer lands mid-turn and the model keeps going. Without a message of its own, that
    /// continuation streams into the assistant message *above* the answer (`stream_msg` still
    /// points there), so everything the model does next renders above what the user just said
    /// and the answer stays pinned to the bottom until the turn ends. Close the old message and
    /// open a fresh one, the way a turn boundary would: the transcript then reads in clock order.
    pub(crate) fn open_continuation_message(&mut self) {
        let tick = self.tick;
        self.main_conv().open_continuation(tick);
    }

    /// Keyboard events. Real-clock version; semantics in [`Chat::on_key_at`].
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.on_key_at(code, modifiers, std::time::Instant::now())
    }

    /// Resets the error state (one of AC-03's four resets: clears the error row / full-screen error state).
    fn dismiss_error(&mut self) {
        self.last_error = None;
    }

    /// #18 full-flow full-screen error-state keys (AC-26/53: the way back is not a dead end):
    /// Enter = retry (reruns the last input), Esc = back, Ctrl+C = quit, the rest ignored.
    fn error_screen_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        match code {
            KeyCode::Enter => {
                self.dismiss_error();
                if !self.last_prompt.is_empty() {
                    self.resubmit(self.last_prompt.clone());
                }
                true
            }
            KeyCode::Esc => {
                self.dismiss_error();
                true
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.ctrl_c(now),
            _ => true,
        }
    }

    /// Keyboard events (`now` is injectable: the Ctrl+C double-press window and paste-burst detection both need a clock).
    ///
    /// Priority, top to bottom: dialog → `/model` menu → history search → interrupt/quit semantics
    /// → editing keys. Esc is the exception: it belongs to no single overlay and walks
    /// [`EscLayer::ORDER`] instead, so its priority can be read in one place rather than
    /// inferred from the order of the handlers below. Returns whether it was consumed.
    pub fn on_key_at(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        let consumed = self.key_at(code, modifiers, now);
        // The receipt folds before the frame that shows it (D154). A keystroke
        // asks the core — submit, execute, wake — and what came back is drawn on
        // the *next* frame, which is this one: folding here rather than on the
        // next tick is the difference between "the console draws what the core
        // published" and a visible lag on every `Enter`.
        self.pump_store();
        self.drain_frames();
        consumed
    }

    fn key_at(&mut self, code: KeyCode, modifiers: KeyModifiers, now: std::time::Instant) -> bool {
        let pasting = self.track_burst(now);
        self.pasting = pasting;
        // The two "immediately after" rules are counted in keys, not seconds
        // (D86): the tick is what makes a kill coalesce with the kill before
        // it and `alt+y` a yank-pop rather than a no-op.
        self.composer.tick();
        // `ctrl+x` arms its chord for exactly one key. Taking it here rather
        // than in `control_key` is what makes "anything else" mean anything
        // else — a plain character, Esc, a dialog key — and not just the
        // control keys that reach the same handler.
        let chord = self.composer.take_chord(now);
        // Update-banner breathing (P1): the first keypress in the window stops it immediately (the user's attention has moved to the input;
        // the banner itself stays, it just stops breathing).
        if self.update_anim_active() {
            self.update_banner_stopped = true;
        }
        // #18 full-flow full-screen error state: primary actions Enter=retry / Esc=back, the rest ignored.
        if let Some(err) = &self.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return self.error_screen_key(code, modifiers, now);
        }
        // Esc dismisses the topmost layer and nothing else (D80): it is judged
        // before the handlers below because every one of them used to claim it,
        // and the resulting order — busy interrupt first — meant Esc killed the
        // turn instead of closing the dropdown the user was looking at.
        if code == KeyCode::Esc {
            return self.escape(now);
        }
        if self.ask_key_at(code, modifiers, now) {
            return true;
        }
        // A printable key that no menu claims (menus only take ↑↓/Enter/Esc/
        // digits/s) closes the menu first, then edits normally. Without this,
        // typing "/theme" over an open /think menu kept feeding a menu the
        // screen no longer showed — Enter landed on an invisible selection.
        if self.menu_open()
            && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(code, KeyCode::Char(c) if !c.is_ascii_digit() && c != 's')
        {
            self.close_menus();
        }
        // `/model` `/think` selectors take priority over input (↑↓/Enter/Esc fully consumed).
        if self.model_menu_key(code, modifiers) {
            return true;
        }
        if self.think_menu_key(code, modifiers) {
            return true;
        }
        if self.theme_menu_key(code, modifiers) {
            return true;
        }
        if self.images_menu_key(code, modifiers) {
            return true;
        }
        if self.resume_menu_key(code, modifiers) {
            return true;
        }
        if self.provider_menu_key(code, modifiers) {
            return true;
        }
        if self.search.is_some() {
            return self.search_key(code, modifiers);
        }
        // The background dialog takes precedence over global editing keys: it
        // is modal for what it uses (D107).
        if self.background_dialog_key(code, modifiers) {
            return true;
        }
        // The rewind selector is modal while it is open (D91): it is a chooser
        // over the composer, and a stray key must not reach the draft.
        if self.rewind_key(code, modifiers) {
            return true;
        }
        // The roster (v6) is the opposite of modal: it claims keys only while
        // a row is selected, and any printable character gives the keyboard
        // straight back to the draft (type-to-exit) — which is what makes a
        // constant list safe to live under the composer.
        if self.roster_key(code, modifiers) {
            return true;
        }
        // Interrupt (busy) and quit (idle) both live on Ctrl+C, judged before editing keys.
        // Unlike Esc, Ctrl+C skips the layer stack: it interrupts with anything open.
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            return self.ctrl_c(now);
        }
        self.notice = None;
        // Composer completion keys take priority over input. The `@` dropdown
        // is judged first: it is anchored at the caret, and the two are
        // mutually exclusive anyway (D85).
        if !self.bash_mode && self.mention_menu_key(code, modifiers) {
            return true;
        }
        // Slash dropdown keys (Tab completes / Esc closes / ↑↓ navigate) take priority over input.
        if !self.bash_mode && self.slash_menu_key(code, modifiers) {
            return true;
        }
        if modifiers.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c) = code
        {
            return self.control_key(c, chord, now);
        }
        if modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(c) = code
        {
            return self.alt_key(c);
        }
        match code {
            // Shift+Tab: cycle the permission mode (CC app:cyclePermissionMode).
            KeyCode::BackTab => {
                // On an away page, shift+tab cycles the **viewed agent's**
                // permission mode and leaves the console's alone (the zoom's
                // rule, which is CC's).
                if !self.active.is_main() {
                    self.cycle_zoom_permission_mode();
                } else {
                    self.cycle_permission_mode();
                }
                true
            }
            KeyCode::Left => {
                self.cursor = crate::tui::input::prev_char(&self.input, self.cursor);
                true
            }
            KeyCode::Right => {
                self.cursor = crate::tui::input::next_char(&self.input, self.cursor);
                true
            }
            KeyCode::Home => {
                self.cursor = crate::tui::input::line_start(&self.input, self.cursor);
                true
            }
            KeyCode::End => {
                self.cursor = crate::tui::input::line_end(&self.input, self.cursor);
                true
            }
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            // Alt+Backspace is readline's `backward-kill-word`: the sub-word
            // kill, so it stops inside a path where ctrl+w takes the whole
            // token (D86).
            KeyCode::Backspace if modifiers.contains(KeyModifiers::ALT) => {
                self.snapshot(EditKind::Bulk);
                let end = self.cursor;
                let start = crate::tui::input::subword_left(&self.input, end);
                let cut =
                    crate::tui::input::kill_between(&mut self.input, &mut self.cursor, start, end);
                self.composer.kill(cut, KillDir::Back);
                self.after_edit();
                true
            }
            KeyCode::Backspace => {
                // Empty-input backspace in bash mode exits shell mode (CC).
                if self.bash_mode && self.input.is_empty() {
                    self.bash_mode = false;
                    return true;
                }
                self.snapshot(EditKind::Delete);
                crate::tui::input::backspace(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            KeyCode::Delete => {
                self.snapshot(EditKind::Delete);
                crate::tui::input::delete(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            KeyCode::Tab if self.bash_mode => {
                self.complete_bash_history();
                true
            }
            // Shift+Enter (available when the terminal reports enhanced keyboards) and pasted Enter are both newlines.
            KeyCode::Enter
                if pasting || modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert_newline();
                // Only pasted newlines can pile up large text → fold into a placeholder at the threshold.
                if pasting {
                    self.collapse_paste();
                }
                true
            }
            KeyCode::Enter => {
                // `\` + Enter: the newline every terminal can type (CC).
                if self.input.ends_with('\\') && self.cursor == self.input.len() {
                    self.snapshot(EditKind::Bulk);
                    self.input.pop();
                    self.cursor = self.input.len();
                    self.insert_newline();
                    return true;
                }
                self.submit();
                true
            }
            // `?` on empty input toggles the shortcut panel; with text it is an ordinary character.
            // Inside a paste it is always the character: a payload that opens
            // with `?` or `!` is text, not a command to this composer (D86).
            KeyCode::Char('?') if self.input.is_empty() && !self.bash_mode && !pasting => {
                self.help_visible = !self.help_visible;
                true
            }
            // `!` on empty input enters shell mode (`!` itself never enters the input).
            KeyCode::Char('!') if self.input.is_empty() && !self.bash_mode && !pasting => {
                self.bash_mode = true;
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.snapshot(EditKind::Insert);
                let mut buf = [0u8; 4];
                crate::tui::input::insert(
                    &mut self.input,
                    &mut self.cursor,
                    c.encode_utf8(&mut buf),
                );
                self.after_edit();
                true
            }
            KeyCode::PageDown => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_add(10);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            KeyCode::PageUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(10);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            _ => false,
        }
    }

    /// Paste-burst detection: [`PASTE_BURST_KEYS`] consecutive key presses with intervals under
    /// [`PASTE_BURST_GAP`] count as a paste (limitations in that constant's comment).
    fn track_burst(&mut self, now: std::time::Instant) -> bool {
        let fast = self
            .last_key_at
            .is_some_and(|last| now.duration_since(last) < PASTE_BURST_GAP);
        self.burst_keys = if fast { self.burst_keys + 1 } else { 0 };
        self.last_key_at = Some(now);
        self.burst_keys >= PASTE_BURST_KEYS
    }

    /// Ctrl+C: interrupts when busy; with text while idle, clears it (into history, retrievable with ↑);
    /// first press on idle empty input shows a hint, a second press within [`CTRL_C_WINDOW`] quits.
    fn ctrl_c(&mut self, now: std::time::Instant) -> bool {
        if self.conv.busy {
            // The quit path below is gated on `busy`, so a turn that never clears it (its
            // task died) used to leave `kill` as the only way out. An interrupt the turn
            // has ignored for [`INTERRUPT_GRACE`] hands Ctrl+C back its exit meaning.
            if self
                .interrupt_at
                .is_some_and(|at| now.duration_since(at) >= INTERRUPT_GRACE)
            {
                self.exit = true;
                return true;
            }
            self.interrupt(now);
            self.notice = Some("Interrupting… press ctrl-c again to force quit");
            self.notice_until = Some(now + CTRL_C_WINDOW);
            return true;
        }
        if !self.input.is_empty() {
            self.clear_input_into_history();
            self.notice = None;
            self.ctrl_c_at = None;
            return true;
        }
        let armed = self
            .ctrl_c_at
            .is_some_and(|at| now.duration_since(at) <= CTRL_C_WINDOW);
        if armed {
            self.exit = true;
            return true;
        }
        self.ctrl_c_at = Some(now);
        self.notice = Some("Press ctrl-c again to exit");
        self.notice_until = Some(now + CTRL_C_WINDOW);
        true
    }

    /// Esc: dismisses the topmost open [`EscLayer`] and stops there — one press,
    /// one level. With nothing open on an idle empty input, esc-esc opens the
    /// rewind selector (D91) — the slot D80 left for it.
    fn escape(&mut self, now: std::time::Instant) -> bool {
        match self.esc_layer() {
            Some(layer) => self.esc_dismiss(layer, now),
            None => self.esc_rewind(now),
        }
    }

    /// esc-esc on an empty idle composer: the second press inside [`ESC_WINDOW`]
    /// opens the rewind selector, mirroring how the same two presses clear a
    /// draft that is not empty.
    ///
    /// No busy check is needed here and none is written: `Interrupt` is a
    /// layer, so under a running turn `esc_layer()` answers before this ever
    /// runs. `open_rewind` keeps its own guards for the paths that do not come
    /// through a key.
    fn esc_rewind(&mut self, now: std::time::Instant) -> bool {
        let armed = self
            .esc_at
            .is_some_and(|at| now.duration_since(at) <= ESC_WINDOW);
        if armed {
            self.esc_at = None;
            self.notice = None;
            self.open_rewind();
            return true;
        }
        self.esc_at = Some(now);
        self.notice = Some("Press esc again to rewind");
        self.notice_until = Some(now + ESC_WINDOW);
        true
    }

    /// The layer Esc acts on right now: the first entry of [`EscLayer::ORDER`]
    /// that is open. Key dispatch and the hint text both read this — they used
    /// to disagree, because the order lived in the shape of an if-chain.
    pub(crate) fn esc_layer(&self) -> Option<EscLayer> {
        EscLayer::ORDER
            .into_iter()
            .find(|layer| self.esc_layer_open(*layer))
    }

    fn esc_layer_open(&self, layer: EscLayer) -> bool {
        match layer {
            EscLayer::AskDialog => self.pending_ask.is_some(),
            EscLayer::Menu => self.menu_open(),
            EscLayer::Rewind => self.rewind.is_some(),
            EscLayer::BackgroundDialog => self.dialog.is_some(),
            EscLayer::SlashDropdown => !self.slash_suggestions.is_empty(),
            EscLayer::MentionDropdown => self.mention.is_some(),
            EscLayer::Search => self.search.is_some(),
            // The full-screen error state is not a layer: it owns the whole
            // canvas and its own keys (`error_screen_key`).
            EscLayer::ErrorRow => self
                .last_error
                .as_ref()
                .is_some_and(|e| e.level != crate::error::ErrorLevel::Full),
            EscLayer::InfoLines => !self.slash_info_lines.is_empty(),
            EscLayer::HelpPanel => self.help_visible,
            EscLayer::Roster => self.roster_selection().is_some(),
            EscLayer::TaskPanel => self.tasks_visible && !self.tasks_auto,
            EscLayer::AwayStop => !self.active.is_main() && self.zoom_is_running(),
            // Main's turn is out of Esc's reach while a page is up (v6): the
            // key on an agent's page must not interrupt a turn the user is
            // not even looking at. Ctrl+C keeps the unconditional interrupt.
            EscLayer::Interrupt => self.conv.busy && self.active.is_main(),
            // Shell mode is the console's and outlives a page turn (D135), so
            // the key that leaves it does too: a mode you turned on is a mode
            // you close before the page under it closes.
            EscLayer::BashMode => self.bash_mode && self.input.is_empty(),
            EscLayer::ClearInput => !self.input.is_empty(),
            EscLayer::AwayHome => !self.active.is_main(),
        }
    }

    /// Closes one layer. A layer with a key handler of its own keeps its close
    /// semantics there (the model picker returns one level at a time, search
    /// must not adopt its hit); the stack only decides which layer hears the key.
    fn esc_dismiss(&mut self, layer: EscLayer, now: std::time::Instant) -> bool {
        const ESC: KeyCode = KeyCode::Esc;
        const NONE: KeyModifiers = KeyModifiers::NONE;
        match layer {
            EscLayer::AskDialog => self.ask_key_at(ESC, NONE, now),
            EscLayer::Menu => {
                self.model_menu_key(ESC, NONE)
                    || self.think_menu_key(ESC, NONE)
                    || self.theme_menu_key(ESC, NONE)
                    || self.resume_menu_key(ESC, NONE)
                    || self.provider_menu_key(ESC, NONE)
                    || self.images_menu_key(ESC, NONE)
            }
            EscLayer::Rewind => self.rewind_key(ESC, NONE),
            EscLayer::BackgroundDialog => self.background_dialog_key(ESC, NONE),
            EscLayer::SlashDropdown => self.slash_menu_key(ESC, NONE),
            EscLayer::MentionDropdown => self.mention_menu_key(ESC, NONE),
            EscLayer::Search => self.search_key(ESC, NONE),
            // A Page/Field error row is dismissable like every other overlay —
            // it used to sit above the prompt until the next turn started.
            EscLayer::ErrorRow => {
                self.last_error = None;
                self.dirty = true;
                true
            }
            EscLayer::InfoLines => {
                self.slash_info_lines.clear();
                self.dirty = true;
                true
            }
            EscLayer::HelpPanel => {
                self.help_visible = false;
                true
            }
            // The roster's rows are constant furniture (v6); the one state Esc
            // can take is the cursor on them.
            EscLayer::Roster => {
                self.roster_sel = None;
                self.dirty = true;
                true
            }
            // The tasks panel opened with ctrl+t closes with Esc (it used to have
            // no exit at all — the ? panel closed, this one squatted).
            EscLayer::TaskPanel => {
                self.tasks_visible = false;
                self.dirty = true;
                true
            }
            // The page's own turn first, the page second — the zoom's ladder,
            // now on the ORDER like everything else.
            EscLayer::AwayStop => {
                if let Some(name) = self.zoomed().map(str::to_string) {
                    self.stop_agent(&name);
                }
                true
            }
            EscLayer::Interrupt => {
                self.interrupt(now);
                true
            }
            EscLayer::BashMode => {
                self.bash_mode = false;
                true
            }
            EscLayer::ClearInput => self.esc_clear_input(now),
            EscLayer::AwayHome => {
                self.switch_to(None);
                true
            }
        }
    }

    /// esc-esc on a non-empty input: the second press inside [`ESC_WINDOW`]
    /// clears the draft into history, retrievable with ↑.
    fn esc_clear_input(&mut self, now: std::time::Instant) -> bool {
        let armed = self
            .esc_at
            .is_some_and(|at| now.duration_since(at) <= ESC_WINDOW);
        if armed {
            self.clear_input_into_history();
            self.esc_at = None;
            self.notice = None;
            return true;
        }
        self.esc_at = Some(now);
        self.notice = Some("Press esc again to clear");
        self.notice_until = Some(now + ESC_WINDOW);
        true
    }

    /// What Esc does to a running turn right now. With a layer stacked above
    /// the interrupt, Esc closes that layer and the turn keeps running — the
    /// status row must not promise otherwise.
    pub(crate) fn esc_busy_hint(&self) -> &'static str {
        match self.esc_layer() {
            Some(EscLayer::Interrupt) | None => "esc to interrupt",
            Some(_) => "esc to close",
        }
    }

    /// The whole hint on the running-status row. Esc is always offered; ctrl+b
    /// joins it only while a foreground shell command is in flight, because that
    /// is the only time it means "background this" (D84).
    pub(crate) fn busy_hint(&self) -> String {
        // On a page, `esc` stops the agent being watched (EscLayer::AwayStop),
        // so the row says which of the key's two meanings it has right now —
        // the D39 rule, applied to the surface D132 gave the row.
        if !self.active.is_main() && self.zoom_is_running() {
            return "esc stops this agent · ↑ home".to_string();
        }
        let esc = self.esc_busy_hint();
        if self.live.running() {
            return format!("{esc} · ctrl+b to run in background");
        }
        esc.to_string()
    }

    /// Interrupts the current turn (Esc / Ctrl+C while busy). The first request is stamped
    /// so Ctrl+C can tell "the turn is stopping" from "the turn is never going to answer".
    fn interrupt(&mut self, now: std::time::Instant) {
        self.conv.interrupted = true;
        self.interrupt_at.get_or_insert(now);
        self.cancel_tx.send_replace(true);
        // The dialog goes with the turn: the user asked for everything in flight
        // to stop, and a dialog is in flight.
        self.cancel_asks(false);
    }

    /// Ctrl+<char> editing commands (readline semantics).
    ///
    /// `chord` says the previous key was `ctrl+x`, which turns this key's
    /// `ctrl+e` into the readline chord for "compose in `$EDITOR`" (D86).
    fn control_key(&mut self, c: char, chord: bool, now: std::time::Instant) -> bool {
        if chord && c == 'e' {
            self.compose_in_editor();
            return true;
        }
        match c {
            'a' => {
                self.cursor = crate::tui::input::line_start(&self.input, self.cursor);
                true
            }
            'e' => {
                self.cursor = crate::tui::input::line_end(&self.input, self.cursor);
                true
            }
            // Kill to end of line. D90 spent this key on the conversation
            // switcher and moved the kill to alt+k; D103 retires the switcher
            // with the conversations it switched between, so readline's own
            // binding comes back. `alt+k` stays as its alias rather than being
            // taken away a second time from anyone who learned it.
            'k' => {
                self.kill_to_end();
                true
            }
            'u' => {
                // Empty-input ctrl+u in bash mode exits shell mode (CC).
                if self.bash_mode && self.input.is_empty() {
                    self.bash_mode = false;
                    return true;
                }
                self.snapshot(EditKind::Bulk);
                let cut = crate::tui::input::kill_to_start(&mut self.input, &mut self.cursor);
                self.composer.kill(cut, KillDir::Back);
                self.after_edit();
                true
            }
            'w' => {
                self.snapshot(EditKind::Bulk);
                let cut = crate::tui::input::kill_word(&mut self.input, &mut self.cursor);
                self.composer.kill(cut, KillDir::Back);
                self.after_edit();
                true
            }
            'y' => {
                self.yank();
                true
            }
            // ctrl+x arms the `ctrl+x ctrl+e` chord and does nothing on its
            // own. The next key clears it wherever it lands.
            'x' => {
                self.composer.arm_chord(now);
                true
            }
            // ctrl+p/ctrl+n are ↑/↓ exactly — same function, so history
            // browsing and D83's queue pull-back behave identically.
            'p' => self.vertical(false),
            'n' => self.vertical(true),
            // ctrl+d deletes the char after the caret only when there is text (empty input never quits).
            'd' => {
                if self.input.is_empty() {
                    return true;
                }
                self.snapshot(EditKind::Delete);
                crate::tui::input::delete(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            'j' => {
                self.insert_newline();
                true
            }
            // Ctrl+G composes the draft in `$EDITOR` (D86). It used to open
            // the agents/channels workspace, which D89 retired and whose
            // question the ctrl+b background dialog answers now.
            'g' => {
                self.compose_in_editor();
                true
            }
            'l' => {
                self.force_redraw = true;
                self.dirty = true;
                true
            }
            // Ctrl+O opens the transcript view (D82). A question on screen keeps
            // priority: the pager would bury a dialog that is blocking a turn,
            // and the answer is one keystroke away.
            'o' => {
                if self.pending_ask.is_none() {
                    self.open_transcript = true;
                    self.dirty = true;
                }
                true
            }
            'r' => {
                self.open_search();
                true
            }
            's' => {
                self.toggle_stash();
                true
            }
            // ctrl+t is the task panel's key and only its (D115, user
            // ruling: "ctrl+t 只和 task 展示有关"): the tasks in flight,
            // toggled. D104's second stop — the agent tree — retired from the
            // cycle because the tree already had a better door, `shift+↑/↓`,
            // and the pills name it on every frame; a stop that duplicated a
            // named door was a stop spent on nothing.
            't' => {
                if self.tasks_visible {
                    self.tasks_visible = false;
                    self.tasks_auto = false;
                } else {
                    self.tasks_visible = true;
                    // Manually opened: keep the panel even when everything is done (the user explicitly wants to see it).
                    self.tasks_auto = false;
                    self.refresh_tasks();
                }
                self.dirty = true;
                true
            }
            // Ctrl+_ arrives as byte 0x1F, which crossterm reports as Ctrl+7; terminals with the enhanced
            // keyboard protocol report `_` or `/` — all three count as undo.
            '7' | '_' | '/' => {
                self.undo_edit();
                true
            }
            _ => false,
        }
    }

    /// Alt+<char>: sub-word movement and kills, yank-pop, and the thinking
    /// toggle. The motions here are readline's `backward-word` family, so they
    /// stop inside a path (D86); `ctrl+w` keeps the whitespace word a shell has.
    fn alt_key(&mut self, c: char) -> bool {
        match c {
            'b' => {
                self.cursor = crate::tui::input::subword_left(&self.input, self.cursor);
                true
            }
            'f' => {
                self.cursor = crate::tui::input::subword_right(&self.input, self.cursor);
                true
            }
            'd' => {
                self.snapshot(EditKind::Bulk);
                let start = self.cursor;
                let end = crate::tui::input::subword_right(&self.input, self.cursor);
                let cut =
                    crate::tui::input::kill_between(&mut self.input, &mut self.cursor, start, end);
                self.composer.kill(cut, KillDir::Forward);
                self.after_edit();
                true
            }
            // The alias D90 created for the kill, kept beside alt+d, its
            // sibling in the ring: same kill, same ring, same direction, so
            // consecutive forward kills still coalesce in text order.
            'k' => {
                self.kill_to_end();
                true
            }
            'y' => self.yank_pop(),
            't' => {
                self.toggle_thinking();
                true
            }
            _ => false,
        }
    }

    /// Ctrl+Y: insert the top of the kill ring at the caret.
    fn yank(&mut self) {
        let Some(text) = self.composer.top().map(str::to_string) else {
            return;
        };
        self.snapshot(EditKind::Bulk);
        let start = self.cursor;
        crate::tui::input::insert(&mut self.input, &mut self.cursor, &text);
        self.composer.note_yank(start, text.len());
        self.after_edit();
    }

    /// Alt+Y: rotate the ring in place over the text the previous key yanked.
    /// Anywhere else it is not a binding at all — it does nothing and says
    /// nothing, which is what readline does with `yank-pop` out of context.
    fn yank_pop(&mut self) -> bool {
        let Some((start, len, text)) = self.composer.yank_pop() else {
            return false;
        };
        self.snapshot(EditKind::Bulk);
        let end = (start + len).min(self.input.len());
        let start = start.min(end);
        self.input.replace_range(start..end, &text);
        self.cursor = start + text.len();
        self.after_edit();
        true
    }

    /// Ctrl+G / `ctrl+x ctrl+e`: ask the host for the `$EDITOR` round trip.
    /// A pending question keeps priority for the same reason ctrl+o does — the
    /// dialog is blocking a turn and its keys are one keystroke from done.
    fn compose_in_editor(&mut self) {
        if self.pending_ask.is_some() {
            return;
        }
        self.open_editor = true;
        self.dirty = true;
    }

    /// ↑/↓: move within a multi-line input first, then switch history at the first/last row;
    /// ↑ while busy with a queue pulls back the last queued message.
    fn vertical(&mut self, down: bool) -> bool {
        // Pulling back a queued message only happens on empty input: what is being typed should not be clobbered.
        if !down && self.conv.busy && self.input.is_empty() && !self.page_queue().is_empty() {
            // The turn may take this one first (D83). Whichever reached the actor
            // first wins, and a pull-back that lost is a no-op: the text is in the
            // request by then, so bringing it back into the composer would send it
            // twice.
            self.intend(crate::tui::intent::Intent::ReclaimTail(self.active.clone()));
            return true;
        }
        let width = self.input_width();
        if let Some(cursor) = crate::tui::input::move_row(&self.input, self.cursor, width, down) {
            self.cursor = cursor;
            return true;
        }
        let next = if down {
            self.history.newer()
        } else {
            self.history.older(&self.input)
        };
        match next {
            Some(text) => {
                self.snapshot(EditKind::Bulk);
                self.input = text;
                self.cursor = self.input.len();
                self.update_slash_suggestions();
                true
            }
            None => {
                // v6: at the bottom of history, `↓` falls into the roster —
                // CC's three-level fallthrough (cursor → history → rows), so
                // the list needs no key of its own.
                if down {
                    self.roster_enter_selection();
                }
                true
            }
        }
    }

    /// Available input width (terminal width - 2 prefix columns - right padding).
    pub fn input_width(&self) -> usize {
        self.width.saturating_sub(4).max(8)
    }

    /// Newline insertion (`\`+Enter / Ctrl+J / Shift+Enter / Enter inside a paste).
    fn insert_newline(&mut self) {
        self.snapshot(EditKind::Bulk);
        crate::tui::input::insert(&mut self.input, &mut self.cursor, "\n");
        self.after_edit();
    }

    /// Replaces the whole input and puts the caret at the end.
    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.cursor = self.input.len();
        self.update_slash_suggestions();
    }

    /// Clears the input and records it in history (Ctrl+C / double Esc: retrievable with ↑).
    fn clear_input_into_history(&mut self) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.record_history(&text);
        self.update_slash_suggestions();
    }

    /// Kill from the cursor to the end of the logical line, into the ring.
    /// One body, two keys: `ctrl+k` (readline's own, back from D90's switcher)
    /// and `alt+k` (the alias D90 created, kept so nobody loses a binding).
    fn kill_to_end(&mut self) {
        self.snapshot(EditKind::Bulk);
        let cut = crate::tui::input::kill_to_end(&mut self.input, &mut self.cursor);
        self.composer.kill(cut, KillDir::Forward);
        self.after_edit();
    }

    /// Wrap-up after every edit: refresh the dropdown suggestions, leave history-browsing mode.
    ///
    /// While the edit is a paste rather than typing ([`Chat::pasting`], D86)
    /// the completion surfaces are closed instead of recomputed. A dropdown is
    /// an answer to typing, and pasted text that happens to contain `@` or `/`
    /// is not asking the question — worse, an `@` dropdown opened mid-burst
    /// claims the Enter that the rest of the paste needs as a newline, and the
    /// funnel's file walk would run once per pasted character. The first
    /// keystroke after the burst re-evaluates, which is the only moment the
    /// end of a burst is observable at all.
    pub(crate) fn after_edit(&mut self) {
        self.history.detach();
        if self.pasting {
            self.clear_slash_suggestions();
            self.mention = None;
        } else {
            self.update_slash_suggestions();
        }
        // G12: error/usage rows clear on the next input — the user has acted on them.
        if !self.slash_error_lines.is_empty() {
            self.slash_error_lines.clear();
            self.slash_error_at = None;
            self.dirty = true;
        }
        // Info output follows the same rule: reading time until the user acts.
        if !self.slash_info_lines.is_empty() {
            self.slash_info_lines.clear();
            self.dirty = true;
        }
    }

    /// Records and persists a prompt. A write failure only degrades to in-session history (once,
    /// no repeated retries).
    pub(crate) fn record_history(&mut self, text: &str) {
        if !self.history.record(text) || !self.history_writable {
            return;
        }
        let path = std::path::PathBuf::from(&self.cwd);
        if crate::tui::history::save(&self.session.home, &path, self.history.entries()).is_err() {
            self.history_writable = false;
        }
    }

    /// Undo stack: consecutive inserts merge into one step; deletes/whole replacements are their own steps.
    pub(crate) fn snapshot(&mut self, kind: EditKind) {
        let coalesce =
            kind != EditKind::Bulk && self.last_edit == Some(kind) && !self.undo.is_empty();
        self.last_edit = Some(kind);
        if coalesce {
            return;
        }
        self.undo.push((self.input.clone(), self.cursor));
        if self.undo.len() > UNDO_MAX {
            self.undo.remove(0);
        }
    }

    /// Ctrl+_: returns to the previous step's text and caret.
    pub(crate) fn undo_edit(&mut self) {
        let Some((text, cursor)) = self.undo.pop() else {
            return;
        };
        self.input = text;
        self.cursor = cursor.min(self.input.len());
        self.last_edit = None;
        self.update_slash_suggestions();
    }

    /// Ctrl+S: with text, stash and clear it; on empty input, restore (including the caret).
    fn toggle_stash(&mut self) {
        if self.input.is_empty() {
            if let Some((text, cursor)) = self.stash.take() {
                self.input = text;
                self.cursor = cursor.min(self.input.len());
                self.update_slash_suggestions();
                self.notice = Some("draft restored");
                self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
            }
            return;
        }
        let replaced = self.stash.is_some();
        self.stash = Some((std::mem::take(&mut self.input), self.cursor));
        self.cursor = 0;
        self.last_edit = None;
        self.update_slash_suggestions();
        self.notice = Some(if replaced {
            "draft saved (old draft overwritten) · ctrl+s on an empty input restores it"
        } else {
            "draft saved · ctrl+s on an empty input restores it"
        });
        self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
    }

    /// Shift+Tab: default → acceptEdits → plan → default.
    /// bypassPermissions / dontAsk stay in the cycle only when the session started in that mode
    /// (dangerous modes must not be reachable by one mispress).
    ///
    /// The new mode is applied to the core, which is where the mode a run
    /// obeys is read from (D154). It used to be set on the console alone, so
    /// `config/read` and the badge could say two different things about one
    /// session.
    fn cycle_permission_mode(&mut self) {
        let next = crate::tui::selection::next_permission_mode(
            self.permission_mode(),
            self.session.permission_mode,
        );
        self.apply_to_core(crate::app::command::Action::PermissionModeSet {
            mode: crate::tui::selection::app_permission_mode(next),
        });
    }

    /// Alt+T: thinking toggle (off ↔ the last non-off level, default medium).
    fn toggle_thinking(&mut self) {
        let current = self.thinking();
        let next = match current.as_deref() {
            None | Some("off") => self
                .last_thinking
                .clone()
                .unwrap_or_else(|| "medium".into()),
            Some(level) => {
                self.last_thinking = Some(level.to_string());
                "off".to_string()
            }
        };
        self.slash_think(&next);
    }

    /// bash-mode Tab: prefix-completes from the `!` commands run in this session.
    fn complete_bash_history(&mut self) {
        let prefix = self.input.clone();
        let Some(hit) = self
            .bash_history
            .iter()
            .rev()
            .find(|cmd| cmd.starts_with(&prefix) && cmd.as_str() != prefix)
            .cloned()
        else {
            return;
        };
        self.set_input(hit);
    }

    /// Ctrl+R: enters reverse history search (an empty query hits the most recent entry first).
    fn open_search(&mut self) {
        self.close_menus();
        let mut search = HistorySearch::default();
        if let Some((index, hit)) = self.history.search("", None) {
            search.index = Some(index);
            search.hit = Some(hit);
        }
        self.search = Some(search);
        self.clear_slash_suggestions();
    }

    /// Search-mode keys: typing filters, Ctrl+R takes an older hit, Tab/Esc adopt and keep editing,
    /// Enter adopts and submits, Ctrl+C cancels and restores. Returns consumed (always true).
    fn search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(mut search) = self.search.take() else {
            return false;
        };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('r') if ctrl => {
                if let Some((index, hit)) = self.history.search(&search.query, search.index) {
                    search.index = Some(index);
                    search.hit = Some(hit);
                }
                self.search = Some(search);
            }
            KeyCode::Char('c') if ctrl => {}
            KeyCode::Char(c) if !c.is_control() && !ctrl => {
                search.query.push(c);
                match self.history.search(&search.query, None) {
                    Some((index, hit)) => {
                        search.index = Some(index);
                        search.hit = Some(hit);
                    }
                    None => {
                        search.index = None;
                        search.hit = None;
                    }
                }
                self.search = Some(search);
            }
            KeyCode::Backspace => {
                search.query.pop();
                match self.history.search(&search.query, None) {
                    Some((index, hit)) => {
                        search.index = Some(index);
                        search.hit = Some(hit);
                    }
                    None => {
                        search.index = None;
                        search.hit = None;
                    }
                }
                self.search = Some(search);
            }
            KeyCode::Enter => {
                match search.hit {
                    Some(hit) => {
                        self.set_input(hit);
                        self.submit();
                    }
                    // No match: keep the search layer open (it used to close
                    // silently, eating the Enter).
                    None => self.search = Some(search),
                }
            }
            KeyCode::Tab => {
                if let Some(hit) = search.hit {
                    self.set_input(hit);
                }
            }
            // Esc = cancel, like every other layer (it used to ADOPT the hit —
            // the only place in the app where Esc committed something).
            KeyCode::Esc => {}
            _ => self.search = Some(search),
        }
        true
    }

    /// tick: independent timing for spinner frames and running-state thinking.
    ///
    /// Only set dirty when some row changes with the tick: rebuilding the whole document on idle
    /// equals a 30fps full re-layout, wasting CPU and forcing the host to repaint the viewport every frame.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.has_dynamic_rows() {
            self.dirty = true;
        }
        // The room page follows its log (D134): appended when its conversation
        // moved, closed when its subject left. A no-change tick costs one
        // fingerprint.
        self.sync_away();
        // `meter` (D87): aim the status row's token readout at the live count;
        // a repeat target is a no-op, so this costs one comparison per frame.
        // Main's count: the meter eases main's row, and a page reports its own
        // instance's figure directly (D132).
        let tokens = self.main_conv().output_tokens;
        self.token_meter.retarget(tokens, self.tick, self.motion);
        // The terminal title's working animation (D79 machinery, D87 cadence):
        // one frame per 960ms, and `set_title` drops a repeat, so a busy turn
        // costs about one OSC 2 write per second. A pending permission prompt
        // owns the title while it is up — it is the more urgent state.
        if self.main_conv().busy && self.pending_ask.is_none() {
            let glyph = self.motion.title_glyph(self.tick);
            self.notify
                .set_title(crate::tui::notify::Title::Busy(glyph));
        }
        // The registry (agent states, channel counts) follows on a slow poll;
        // repainting only when the store's own entries change.
        if self.tick.is_multiple_of(15) {
            self.refresh_conversations();
            self.observe_badges();
        }
        // The digest debounce (D98) runs every frame: it is two integer
        // comparisons against a length the domain already keeps, and the thing it
        // decides — whether the room has stopped talking — is a question about
        // *this* frame.
        self.digest_mail();
        // The bottom notice expires with the window it advertises.
        if let Some(until) = self.notice_until
            && std::time::Instant::now() >= until
        {
            self.notice = None;
            self.notice_until = None;
            self.dirty = true;
        }
        // Slash transient hints expire (operation confirmations leave no permanent placeholder);
        // error/usage rows live longer (G12) — they additionally clear on the next input.
        if let Some(at) = self.slash_at
            && at.elapsed() > SLASH_OUTPUT_TTL
        {
            self.slash_lines.clear();
            self.slash_at = None;
            self.dirty = true;
        }
        if let Some(at) = self.slash_error_at
            && at.elapsed() > SLASH_OUTPUT_ERROR_TTL
        {
            self.slash_error_lines.clear();
            self.slash_error_at = None;
            self.dirty = true;
        }
        for msg in &mut self.conv.messages {
            for act in &mut msg.activities {
                if let ActivityKind::Thinking(t) = &mut act.kind
                    && t.state == ThinkingState::Running
                {
                    t.duration_ms = self
                        .tick
                        .saturating_sub(t.start_tick)
                        .saturating_mul(crate::tui::motion::TICK_MS);
                }
            }
        }
        self.sample_dispatches();
        self.absorb_arrivals();
    }

    /// Refresh what every running dispatch row shows of its instance (D106).
    ///
    /// The registry is the only place the numbers live and it **drops them the
    /// instant a run ends** — `spawn_agent_loop` clears the progress cell one
    /// line before it reports `Done` — so the row has to keep its own copy, and
    /// the copy has to survive the frame in which the cell went empty. It does
    /// because the copy only ever grows: within one run the counts are monotone
    /// and a `max` is therefore exact, and a new run gets a new label and
    /// therefore a new row.
    ///
    /// Sampled in the tick beside the thinking clock, for the same reason and
    /// with the same safety: a message holding a running activity never settles,
    /// so nothing written here can reach scrollback.
    fn sample_dispatches(&mut self) {
        let running = self
            .conv
            .messages
            .iter()
            .any(|m| m.activities.iter().any(is_running_dispatch));
        if !running {
            return;
        }
        let roster = self.tree_instances();
        for msg in &mut self.conv.messages {
            for act in &mut msg.activities {
                let ActivityKind::Watch(w) = &mut act.kind else {
                    continue;
                };
                if w.kind != crate::watch::WatchKind::Agent || w.status.is_terminal() {
                    continue;
                }
                let name = crate::tui::activities::watch_instance(&w.label);
                let Some(status) = roster.iter().find(|s| s.name == name) else {
                    continue;
                };
                if !status.recent_activity.is_empty() {
                    w.progress = status
                        .recent_activity
                        .iter()
                        .rev()
                        .take(crate::tui::activities::PROGRESS_LINES)
                        .rev()
                        .cloned()
                        .collect();
                }
                let seen = w.run_stats.unwrap_or_default();
                w.run_stats = Some(crate::tui::activities::RunStats {
                    tool_uses: seen.tool_uses.max(status.tool_uses as usize),
                    tokens: seen.tokens.max(status.output_tokens),
                });
            }
        }
    }

    /// Count the direct messages that landed for main, per sender (D114).
    ///
    /// D106 printed one `@name❯` transcript line per arrival; the inbox turn
    /// removed it — main's mail is main's business, and the flow's whitelist
    /// is the user's own conversation. What the user gets instead is the
    /// status layer's dot on the sender, fed from this count and cleared when
    /// that agent's zoom is visited. The wake and its debounce are untouched:
    /// this reads a mirror of the inbox, never the inbox itself.
    fn absorb_arrivals(&mut self) {
        self.intend_once(crate::tui::intent::Intent::Arrivals);
    }

    /// Frame number within the update-banner breathing window (animation running → Some; no banner / motion off /
    /// stopped by a keypress / window passed → None, resting). The 270-frame window = 9s = 3 breaths.
    fn update_banner_frame(&self) -> Option<u64> {
        if self.update_banner.is_none() || self.motion.off() || self.update_banner_stopped {
            return None;
        }
        let frame = self.tick.saturating_sub(self.update_banner_start);
        (frame < UPDATE_BANNER_FRAMES).then_some(frame)
    }

    /// Whether the update-banner breathing is active (the frame loop keeps dirty set; outside the window it returns to idle).
    pub(crate) fn update_anim_active(&self) -> bool {
        self.update_banner_frame().is_some()
    }

    /// Whether any row changes with the tick (spinner frames / elapsed time / status rows).
    /// false when idle — the tick neither rebuilds the doc nor wakes the component.
    pub fn has_dynamic_rows(&self) -> bool {
        self.conv.busy
            || self.conv.messages.iter().any(|m| {
                m.groups.iter().any(|g| g.active) || m.activities.iter().any(|a| a.is_running())
            })
            || (self.tasks_visible
                && self
                    .tasks_cache
                    .iter()
                    .any(|t| t.status == TodoStatus::InProgress))
            // The background dialog's rows move on their own — a running
            // agent's activity, an idle one's `Idle for 14s`, a shell's
            // runtime — so an open dialog with anybody on it keeps the clock
            // awake, exactly as the tree does (D107).
            || (self.dialog.is_some()
                && (!self.store.view().agents().is_empty()
                    || !self.store.view().commands().is_empty()))
            // The roster counts seconds — a running row's activity and an
            // idle row's `Idle for 14s` both move without an event arriving —
            // so it keeps the clock awake while anybody exists to be shown
            // (v6: the rows are constant furniture).
            || self.roster_len() > 0
            || self.update_anim_active()
            || self.settling()
    }

    /// Whether the host's tick loop has work to do. Returns false when idle so the host skips the whole frame —
    /// with no animation and no pending events, not a single byte is written.
    pub fn needs_tick(&self) -> bool {
        self.has_dynamic_rows()
            || self.slash_at.is_some()
            || self.slash_error_at.is_some()
            || self.notice_until.is_some()
            || !self.events_rx.is_empty()
            || self.store.view().has_interactions()
            // Mail landing in a fully idle session is the one thing that has to
            // wake the clock rather than ride an event: nothing else is
            // happening, and the digest window has to be able to expire (D98).
            || self.session.channels.has_main_mail()
    }

    /// Task-area data source: live snapshot of the on-disk store.
    pub fn tasks(&self) -> Vec<TodoItem> {
        self.session
            .tasks
            .list_ui()
            .into_iter()
            .map(|t| {
                let status = match t.status {
                    crate::tasks::TaskStatus::Pending => TodoStatus::Pending,
                    crate::tasks::TaskStatus::InProgress => TodoStatus::InProgress,
                    crate::tasks::TaskStatus::Completed => TodoStatus::Done,
                };
                TodoItem {
                    id: t.id,
                    text: t.subject,
                    status,
                    owner: t.owner,
                    blocked_by: t.blocked_by,
                }
            })
            .collect()
    }

    /// Refreshes the task cache (disk snapshot; called on the tick cadence and after draining events).
    /// Only set dirty when the snapshot changes — row-count changes alter the canvas height, and the render
    /// layer's shape detection triggers a full repaint.
    pub fn refresh_tasks(&mut self) {
        let next = self.tasks();
        if next != self.tasks_cache {
            self.tasks_cache = next;
            self.dirty = true;
        }
        // Auto-opened task area: hide once everything is done (work over, panel leaves),
        // push a 2s transient line for closure + a way back; manually opened panels stay.
        if self.tasks_auto
            && self.tasks_visible
            && !self.tasks_cache.is_empty()
            && self
                .tasks_cache
                .iter()
                .all(|t| t.status == TodoStatus::Done)
        {
            self.tasks_visible = false;
            self.tasks_auto = false;
            let total = self.tasks_cache.len();
            self.push_slash_output(format!("✓ {total}/{total} tasks done · ctrl+t to view"));
        }
    }

    /// Keep at most this many trailing done items; older ones fold into `… N done`.
    const DONE_SHOWN: usize = 3;
    /// Active-item window size; overflow folds into `… +N more`.
    const TODO_SHOWN: usize = 5;

    /// Task-area rows (CC TaskListV2 placement: above the input).
    /// Shown when the expand signal is set and tasks exist; auto-opened lists hide when everything is done
    /// (wrapped up in `refresh_tasks`); manually opened ones stay.
    pub fn task_lines(&self) -> Vec<Line> {
        if !self.tasks_visible {
            return Vec::new();
        }
        let t = &self.tasks_cache;
        if t.is_empty() {
            return Vec::new();
        }
        let theme = &self.theme;
        let mut out = Vec::new();
        // Header: `{spinner}todo · N/M tasks`
        let mut header = Line::empty();
        if t.iter().any(|i| i.status == TodoStatus::InProgress) {
            header.push_styled(
                format!("{} ", self.motion.pulse(self.tick)),
                SegStyle::fg(theme.claude),
            );
        }
        header.push_styled("todo".to_string(), theme.text());
        let done = t.iter().filter(|i| i.status == TodoStatus::Done).count();
        header.push_styled(
            format!(" · {done}/{} tasks", t.len()),
            SegStyle::fg(theme.text_secondary),
        );
        out.push(header);
        let done_indices: Vec<usize> = t
            .iter()
            .enumerate()
            .filter(|(_, i)| i.status == TodoStatus::Done)
            .map(|(i, _)| i)
            .collect();
        let shown_done = done_indices.len().min(Self::DONE_SHOWN);
        let hidden_done = done_indices.len() - shown_done;
        if hidden_done > 0 {
            out.push(Line::styled(
                format!("… {} done", hidden_done),
                SegStyle::fg(theme.text_secondary),
            ));
        }
        for &idx in done_indices.iter().skip(hidden_done) {
            // `☒` + struck-through text (real strikethrough + dim, see Theme::strikethrough).
            let mut line = Line::styled("☒ ", theme.task_done());
            line.push_styled(t[idx].text.clone(), theme.strikethrough());
            out.push(line);
        }
        // Who is still standing, for the owner suffix: a name in the store is a
        // string, and only a name the registry answers to earns a colour and a
        // row of its own (CC gates the same way on `ownerActive`,
        // `TaskListV2.tsx:268`).
        let roster = self.tree_instances();
        // What a blocker being "open" means: the task it names is not done.
        // Nothing else — this is a readout, not a scheduler.
        let unresolved: Vec<&str> = t
            .iter()
            .filter(|item| item.status != TodoStatus::Done)
            .map(|item| item.id.as_str())
            .collect();
        let palette = crate::tui::avatar::Palette::new(theme);
        let gutter = crate::tui::avatar::Gutter::new(false, false, &palette, &self.faces_pinned);
        let active: Vec<&TodoItem> = t.iter().filter(|i| i.status != TodoStatus::Done).collect();
        for item in active.iter().take(Self::TODO_SHOWN) {
            let blockers: Vec<String> = item
                .blocked_by
                .iter()
                .filter(|id| unresolved.contains(&id.as_str()))
                .map(|id| format!("#{id}"))
                .collect();
            // `☐` not done; in-progress items use the primary accent color for the whole row (CC's active-item highlight).
            // A blocked row is dim whatever its status: it is not what is
            // happening now (`TaskListV2.tsx:322`).
            let style = if !blockers.is_empty() {
                SegStyle::fg(theme.text_secondary)
            } else {
                match item.status {
                    TodoStatus::Pending => theme.task_open(),
                    TodoStatus::InProgress => SegStyle::fg(theme.claude).bold(),
                    TodoStatus::Done => unreachable!("filtered"),
                }
            };
            let mut line = Line::styled("☐ ", style);
            line.push_styled(item.text.clone(), style);
            if let Some(owner) = item.owner.as_deref()
                && let Some(status) = roster.iter().find(|status| status.name == owner)
                && status.state != crate::app::snapshot::AgentState::Stopped
            {
                line.push_styled(" (".to_string(), SegStyle::fg(theme.text_secondary));
                line.push_styled(
                    format!("@{owner}"),
                    SegStyle::fg(palette.avatars[gutter.index_for(owner) % palette.avatars.len()]),
                );
                line.push_styled(")".to_string(), SegStyle::fg(theme.text_secondary));
            }
            if !blockers.is_empty() {
                line.push_styled(
                    format!(" › blocked by {}", blockers.join(", ")),
                    SegStyle::fg(theme.text_secondary),
                );
            }
            out.push(line);
        }
        if active.len() > Self::TODO_SHOWN {
            out.push(Line::styled(
                format!("… +{} more", active.len() - Self::TODO_SHOWN),
                SegStyle::fg(theme.text_secondary),
            ));
        }
        out
    }

    /// Permission-mode label (footer badge).
    pub fn permission_mode_label(&self) -> &'static str {
        match self.permission_mode() {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }

    /// Running status row (ActivityIndicator): when busy, returns the verb + elapsed time + tokens
    /// produced — preferring the running tool (summary/name), then the running
    /// thinking (whimsical word), falling back to "Working". Returns None when idle (row hidden).
    /// The status row of whatever page is on screen (D132), which since D134 is
    /// [`Chat::running_status`] itself.
    ///
    /// It used to be a second reader — the registry's `elapsed`,
    /// `output_tokens` and `recent_activity`, sampled because a page's turn was
    /// not in the console's own state. It is now: the page's store carries the
    /// running tool, the clock the turn started on and the tokens it has
    /// produced, filled by the same events main's is. One reader, one row.
    pub fn page_running_status(&self) -> Option<RunningStatus> {
        self.running_status()
    }

    pub fn running_status(&self) -> Option<RunningStatus> {
        if !self.conv.busy {
            return None;
        }
        let verb = self
            .conv
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| {
                m.activities.iter().find_map(|a| match &a.kind {
                    ActivityKind::Tool(t) if t.status == ToolStatus::Running => {
                        Some(if t.summary.is_empty() {
                            t.name.to_string()
                        } else {
                            t.summary.clone()
                        })
                    }
                    // Running background task/subagent (ActivityIndicator shows the agent activeForm):
                    // the label is `Agent: <description>`.
                    ActivityKind::Watch(w) if w.status == WatchState::Running => {
                        Some(w.label.clone())
                    }
                    ActivityKind::Thinking(t) if t.state == ThinkingState::Running => {
                        Some(t.stage.to_string())
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| "Working".to_string());
        let elapsed = self
            .conv
            .turn_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        Some(RunningStatus {
            verb,
            elapsed,
            // Main's row eases toward its number (D87); a page's reports the
            // count its own turn produced, because the meter tracks one stream
            // and there is one of it (D132's ruling, unchanged).
            tokens: if self.active.is_main() {
                self.token_meter.value(self.tick, self.motion)
            } else {
                self.conv.output_tokens
            },
        })
    }

    pub fn token_rate_label(&self) -> Option<String> {
        if !self.conv.busy {
            return None;
        }
        self.conv
            .token_rate
            .label(std::time::Instant::now(), self.motion.off())
    }

    pub fn context_usage(&self) -> crate::context_usage::ContextUsage {
        self.conv.context_usage
    }

    /// Input-area rendered rows — the single source for the row-count model and rendering:
    /// chrome height is counted from it and assembly emits rows from it.
    ///
    /// No cursor glyph is drawn here: the terminal's own cursor is the only caret
    /// ([`crate::tui::chrome::prompt`] attaches it via `El::Caret`), so it overlays the cell it
    /// sits on and never pushes the text after it aside.
    ///
    /// Empty input gets a one-line dim placeholder; multi-line input beyond [`INPUT_ROWS_MAX`] shows only
    /// the screen around the caret (tail-aligned), so the row count always has an upper bound.
    pub fn prompt_lines(&self) -> Vec<Line> {
        let style = SegStyle::fg(self.theme.text);
        // Search mode: the input row shows the current hit; the query sits in the hint line below.
        if let Some(search) = &self.search {
            let hit = search.hit.clone().unwrap_or_default();
            return vec![Line::styled(one_line(&hit, self.input_width()), style)];
        }
        if self.input.is_empty() {
            // The real cursor rests on the placeholder's first cell; the hint keeps every
            // character (the terminal inverts the cell it covers).
            return vec![Line::styled(
                crate::tui::keys::INPUT_PLACEHOLDER.to_string(),
                self.theme.dim(),
            )];
        }
        let width = self.input_width();
        let lines = crate::tui::input::visual_lines(&self.input, width);
        let (row, _) = crate::tui::input::cursor_cell(&self.input, &lines, self.cursor);
        let start = row.saturating_sub(INPUT_ROWS_MAX - 1);
        lines
            .iter()
            .skip(start)
            .take(INPUT_ROWS_MAX)
            .map(|line| Line::styled(line.text.clone(), style))
            // Each row must occupy exactly one line: history-filled text may contain tabs (folded to spaces),
            // otherwise the column-width math and canvas height would both drift.
            .map(|mut line| {
                line.sanitize();
                line
            })
            .collect()
    }

    /// Poll the domain registries into the conversation engine (D88).
    ///
    /// The store is the accounting a surface with badges would read: how far
    /// the user has read each conversation, whether one wants them, when it
    /// last moved. D104's status layer turned out not to be that surface — CC
    /// puts no badge on a pill or a tree row — so the sweep still does not set
    /// `dirty`, and still costs a registry read and no repaint. The tree keeps
    /// its own clock through `has_dynamic_rows`, because it reads the registry
    /// directly and counts seconds.
    pub fn refresh_conversations(&mut self) {
        let session = self.session.clone();
        self.buffers.refresh(&session, self.tick);
        // A store outlives its page but not its instance: once the registry has
        // forgotten an agent there is no page left to open, and keeping its
        // transcript would grow the console for the length of the session.
        let view = self.store.view();
        self.parked.retain(|key, _| match key {
            crate::ui::ConvKey::Agent(name) => view.agent(name).is_some(),
            crate::ui::ConvKey::Main | crate::ui::ConvKey::Room(_) => true,
        });
    }

    /// Repaint when a badge moved (D115). The pills and the tree are chrome,
    /// rebuilt only on a dirty frame, and the store's slow poll does not know
    /// what the chrome shows — so the poll keeps the badge fingerprint and
    /// dirties the frame the moment it changes. The fingerprint is exactly
    /// what the badge grammar reads: per conversation, its unread and whether
    /// it names you.
    fn observe_badges(&mut self) {
        let print: Vec<(crate::tui::buffer::BufferId, u64, bool)> = self
            .buffers
            .iter()
            .map(|b| (b.id().clone(), b.unread(), b.mention()))
            .collect();
        if print == self.badge_print {
            return;
        }
        // A mention bit turning on rings once (D116's edge detector, v6's
        // ruling on the body): the roster's accent badge is in constant view
        // under the composer, so the bell is the interrupt and the badge is
        // the message — no flow line. Further mentions land behind the same
        // lit badge until the room is read, and none ring while the user is
        // standing in the room, because the store never sets mention on the
        // active conversation.
        let flipped = print.iter().any(|(id, _, mention)| {
            matches!(id, crate::tui::buffer::BufferId::Channel(_))
                && *mention
                && !self.badge_print.iter().any(|(old, _, m)| old == id && *m)
        });
        if flipped {
            self.notify
                .attention(crate::tui::notify::Attention::AgentNotice);
        }
        self.badge_print = print;
        self.dirty = true;
    }

    /// The running command's output so far: dim, indented under the `⎿` row it
    /// belongs to (D84).
    ///
    /// These rows live in the redrawn tail region by construction — a running tool
    /// keeps its message unsettled, so nothing here can reach scrollback, and a
    /// finished command leaves nothing behind to unprint.
    pub(crate) fn bash_tail_rows(&self, width: usize) -> Vec<Line> {
        let Some(tail) = &self.bash_tail else {
            return Vec::new();
        };
        let indent = crate::tui::activities::RESULT_INDENT;
        let mut rows = Vec::new();
        // What is being left out, before what is kept: five lines of five reads
        // very differently from five lines of twelve hundred.
        if tail.total_lines > tail.lines.len() {
            rows.push(Line::styled(
                one_line(&format!("{indent}… {} lines", tail.total_lines), width),
                SegStyle::fg(self.theme.text_secondary),
            ));
        }
        for line in &tail.lines {
            rows.push(Line::styled(
                one_line(&format!("{indent}{line}"), width),
                SegStyle::fg(self.theme.text_secondary),
            ));
        }
        rows
    }

    /// Stop an agent. Every surface that stops one goes through here — the
    /// tree's `k`, the dialog's `x`, the zoom's — so there is one path, one
    /// warning and one watch transition.
    pub(crate) fn stop_agent(&mut self, name: &str) {
        self.intend(crate::tui::intent::Intent::StopAgent(name.to_string()));
    }

    /// What the console does about a stop the core performed.
    pub(crate) fn stopped_agent(
        &mut self,
        name: &str,
        stopped: Result<(Option<crate::watch::WatchId>, usize), String>,
    ) {
        match stopped {
            Ok((watch_id, dropped)) => {
                if let Some(id) = watch_id {
                    self.session.watch.set_state(
                        id,
                        WatchState::Cancelled,
                        Some("stopped".to_string()),
                        None,
                    );
                }
                self.push_warning(if dropped == 0 {
                    format!("stopped {name}")
                } else {
                    format!("stopped {name} · {dropped} queued instructions discarded")
                });
                self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
                self.refresh_conversations();
            }
            Err(error) => self.push_warning(error),
        }
    }

    /// `?` panel rows (single source for the shortcut table). The row budget comes from the terminal height:
    /// the panel must not push the viewport above the terminal height.
    pub fn help_lines(&self) -> Vec<String> {
        if !self.help_visible {
            return Vec::new();
        }
        // Reserve: input 3 rows + footer 1 + a 4-row margin for status/suggestions + 1 safety row.
        let budget = self.height.saturating_sub(9);
        crate::tui::keys::help_lines(self.width.saturating_sub(2), budget)
    }

    /// Queued-message rows (dim `> {text}` below the input); overflow folds into one row.
    pub fn queue_lines(&self) -> Vec<String> {
        let queue = self.page_queue();
        if queue.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = queue
            .entries
            .iter()
            .take(QUEUE_ROWS_MAX)
            .map(|item| format!("> {}", one_line(&item.text, self.width.saturating_sub(4))))
            .collect();
        if queue.len() > QUEUE_ROWS_MAX {
            out.push(format!("… +{} more queued", queue.len() - QUEUE_ROWS_MAX));
        }
        out
    }

    /// The hint under the queued rows, in CC's wording. It is only true while a turn is
    /// running: with nothing in flight the queue is about to submit itself, and there is
    /// no window in which editing it would mean anything.
    pub fn queue_hint(&self) -> Option<&'static str> {
        (self.conv.busy && !self.page_queue().is_empty())
            .then_some("Press up to edit queued messages")
    }

    /// ctrl+r search hint line (`(reverse-i-search)`query': hit`).
    pub fn search_line(&self) -> Option<String> {
        let search = self.search.as_ref()?;
        let (prefix, hit) = match search.hit.as_deref() {
            Some(hit) => ("(reverse-i-search)", hit),
            // bash shows failure explicitly; silence read as "found nothing? or broken?".
            None if !search.query.is_empty() => ("(failed reverse-i-search)", ""),
            None => ("(reverse-i-search)", ""),
        };
        Some(one_line(
            &format!(
                "{prefix}`{}': {hit}   — enter submits · tab accepts · ctrl+r older · esc cancels",
                search.query
            ),
            self.width.saturating_sub(2),
        ))
    }

    /// Scroll/doc consistency: clamp the scroll to the doc end; auto_scroll sticks to the bottom.
    pub fn reconcile_scroll(&mut self, viewport: usize) {
        self.viewport_height = viewport;
        let total = self.doc.rows.len();
        let max_scroll = total.saturating_sub(viewport);
        if self.auto_scroll {
            self.scroll = max_scroll;
        }
        let scroll = self.scroll.min(max_scroll);
        self.scroll = scroll;
        if scroll == max_scroll {
            self.auto_scroll = true;
        }
    }

    /// A message's own static settlement condition (independent of predecessors):
    /// streaming stopped, no running activities, no images loading.
    fn message_static_settled(&self, i: usize) -> bool {
        if Some(i) == self.conv.stream_msg {
            return false;
        }
        let m = &self.conv.messages[i];
        // Images load asynchronously. Settling (and therefore flushing) a
        // message whose images are still in flight would print the
        // `#[image]` fallback rows into the scrollback for good: the kitty
        // sequence is only emitted at flush time, and `build_rows` skips
        // flushed segments, so the picture could never appear. Loads that
        // fail drop out of `images_pending` and settle as the placeholder,
        // which is the intended failure display.
        if !self.images_pending.is_empty()
            && gfx::extract_image_urls(&m.text)
                .iter()
                .any(|url| self.images_pending.contains(url))
        {
            return false;
        }
        !m.groups.iter().any(|g| g.active) && !m.activities.iter().any(|a| a.is_running())
    }

    /// Whether a message is "settled": its rows no longer change (stream stopped, no running activities).
    /// REPL mode: settled messages print into scrollback in one go; unsettled ones stay in the
    /// dynamic tail for in-place redraws. Settling is one-way — once true, the rows never change.
    ///
    /// Sequential settlement: an answer message inserted mid-turn sits after the streaming
    /// assistant message; if a predecessor isn't settled (still streaming / tool running /
    /// image loading), this message must not settle either — flushing past a streaming row
    /// would print an intermediate state into scrollback as unchangeable residue (same
    /// invariant as `streaming_content_is_not_flushed_until_settled`; today's message model
    /// always has predecessors settled, this guard only constrains new scenarios).
    ///
    /// Prefix settlement is monotone (0..=i all settled ⟺ 0..i-1 all settled and i itself
    /// static), so recursing from the previous message is linear — do NOT recurse into every
    /// predecessor: with everything settled that is exponential (freezes the hot path on
    /// every build_rows).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn message_settled(&self, i: usize) -> bool {
        (i == 0 || self.message_settled(i - 1)) && self.message_static_settled(i)
    }

    /// Build the scrolling document: welcome card + messages (text and activities interleaved at their insert points) +
    /// permission-request blocks. The block list is laid out by [`crate::tui::statics::layout`]:
    /// `doc.settled` / checkpoints = the settled prefix (welcome card + all settled messages;
    /// permission-request blocks are never settled).
    ///
    /// In inline mode, segments already flushed ([`Chat::flushed_segments`]) are skipped wholesale:
    /// the doc only covers the dynamic tail, so more flushing means cheaper rebuilds.
    pub fn build_rows(&mut self, width: usize) -> &Doc {
        // The markdown render cache is not width-aware — clear it when the width changes,
        // otherwise message text keeps wrapping at the old width after a resize.
        if self.prev_build_width != width {
            self.prev_build_width = width;
            self.reply_cache.clear();
        }
        self.build_rows_core(width);
        &self.doc
    }

    /// The page's own header line, on every page but main's — the one row that
    /// says whose conversation the screen has become. `None` is main, which
    /// opens with the welcome card instead.
    fn page_label(&self) -> Option<String> {
        match &self.active {
            crate::ui::ConvKey::Main => None,
            crate::ui::ConvKey::Agent(name) => Some(format!("@{name}")),
            crate::ui::ConvKey::Room(name) => Some(format!("#{name}")),
        }
    }

    fn build_rows_core(&mut self, width: usize) {
        let theme = self.theme.clone();
        // The transcript *is* the message store, in order (D103). Segment
        // numbering counts message positions, because those are what the reader
        // sees go by: 0 = welcome card, k+1 = messages[k]. The order is
        // append-only, so the flush cursor keeps meaning what it meant.
        let count = self.conv.messages.len();
        // The clamp is defensive: if the message set is replaced wholesale
        // (/clear, /resume) without the cursor resetting, better to re-render
        // than leave a blank screen.
        let skip = self.flushed_segments.min(count + 1);
        self.tail_start = 0;
        self.mark_base = 0;

        // Prefix-monotone settlement, precomputed in one pass (recursing per
        // message inside the loop would be quadratic on the hot path).
        //
        // One rule for every page since D134. The away page used to need a
        // boundary of its own because it was *rebuilt*: a message could change
        // under the flush cursor, so only the half derived from committed
        // history was safe to freeze. A store fed by events cannot change
        // behind the reader — a running turn holds a running activity, and
        // that is what "not settled" has always meant here.
        let mut settled_flags: Vec<bool> = Vec::with_capacity(count);
        let mut prefix_settled = true;
        let settling = self.settling();
        for i in 0..count {
            // A message inside the `settle` blink is not final yet: its
            // completion row is still wearing the accent, and freezing it now
            // would print that accent into scrollback for good (D87).
            prefix_settled =
                prefix_settled && self.message_static_settled(i) && !(settling && i + 1 == count);
            settled_flags.push(prefix_settled);
        }

        let mut blocks: Vec<Block> = Vec::new();
        if skip == 0 {
            // A page opens with its name as a rule, not the console's
            // welcome card — the one row that says whose page this became.
            if let Some(label) = self.page_label() {
                let color = self.identity_color(label.trim_start_matches(['@', '#']));
                blocks.push(Block::settled(
                    El::Rows(vec![crate::tui::conv::page_header(&label, width, color)]),
                    true,
                ));
            } else {
                blocks.push(Block::settled(self.welcome_el(width, &theme), true));
            }
        }
        let pal = crate::tui::avatar::Palette::new(&theme);
        // The avatar gutter (D97). The pinned table is copied out because the
        // row loop below needs `&mut self` — it is a handful of short strings,
        // next to the theme clone this function already pays for.
        let pinned = self.faces_pinned.clone();
        // The transcript wears the same gutter every view wears (D99): a
        // portrait for main and one for the user. One machinery, one more call
        // site — the console is not a second kind of surface.
        let conversation_gutter = crate::tui::avatar::Gutter::new(
            self.chat_avatars,
            self.image_cap.is_some(),
            &pal,
            &pinned,
        );
        // The faces recorded before the rows are built: the transmit sweep
        // reads `Chat::faces`, and a portrait whose placeholder cells reached
        // the screen without its data is a hole.
        if conversation_gutter.faces {
            let g = &conversation_gutter;
            for index in [
                g.index_for(crate::channels::USER_NAME),
                g.index_for(crate::channels::MAIN_NAME),
            ] {
                self.faces.insert(index);
            }
        }
        let mut spoke: Option<String> = None;
        // Indexed rather than iterated: the body builders below take `&mut
        // self`, so the message cannot be borrowed across the loop.
        #[allow(clippy::needless_range_loop)]
        for i in 0..count {
            let role = self.conv.messages[i].role;
            // Who the last row belonged to, tracked across the whole flow so a
            // portrait is not repeated over every message in a run — and is
            // spent again the moment the other participant speaks.
            // An away page's messages carry their speaker explicitly (v6) —
            // a room's members are not derivable from the text the way the
            // transcript's two participants are; `None` falls back to the
            // marker walk, so main's flow reads exactly as before.
            let named = self.conv.messages[i]
                .speaker
                .clone()
                .or_else(|| speaker_of(role, &self.conv.messages[i].text));
            let previous = std::mem::replace(&mut spoke, named);
            if i + 1 < skip {
                continue;
            }
            let settled = settled_flags[i];
            // Who this row is drawn as — `spoke` was just set to it. The
            // transcript's two participants are the ones they always were, and
            // since D99 they wear the same gutter, so a message looks like
            // itself wherever it is read. A state line has nobody behind it.
            let said = spoke.clone();
            // **Every row takes the gutter; only a speaker takes the face.** A
            // state that gave up the column too would make the message column
            // jog around it, so states align even where they have no portrait.
            let gutter = &conversation_gutter;
            let inner = width.saturating_sub(gutter.width());
            let body = match role {
                Role::User => {
                    // D106's two arrivals — a message from an agent, and the
                    // task notification that reaches main when a run ends —
                    // need the identity palette and the transcript-mode gate,
                    // neither of which the plain user renderer has.
                    let mut rows = El::Rows(
                        self.agent_flow_rows(&self.conv.messages[i].text, inner)
                            .unwrap_or_else(|| {
                                user_message_rows(&self.conv.messages[i].text, inner, &theme)
                            }),
                    );
                    // Send time beside the bubble's first row (D93). A state line
                    // gets none: nothing was sent, and the line is a state, not a
                    // message.
                    // A failed-agent alert (D98) is the exception among state
                    // lines: it *is* news, about someone, at a moment that
                    // matters — "the build broke" reads differently at 09:02 and
                    // at 17:40. The `⚑` mention line (D116) is the second
                    // exception for the same reason — somebody asked for you,
                    // then. The others describe now and have nothing to
                    // stamp; the notification line reports the end of a run
                    // whose own row already carries how long that run took.
                    let time = if crate::tui::chat::is_state_line(&self.conv.messages[i].text)
                        && !crate::tui::bufferview::is_agent_alert(&self.conv.messages[i].text)
                    {
                        String::new()
                    } else {
                        crate::tui::buffer::stamp(self.conv.messages[i].at)
                    };
                    hang_stamp(&mut rows, &time, inner, &theme);
                    rows
                }
                Role::Assistant => self.assistant_el(i, inner, &theme, settled, &pal),
            };
            // The gutter wraps the body: the portrait is two cells tall, so its
            // second row rides the message's first body line (D97). Message
            // block spacing (CC marginTop=1) — one blank row before each
            // message — stays outside it, so a portrait never sits beside
            // nothing.
            let cells = match &said {
                Some(who) if gutter.faces => {
                    let index = gutter.index_for(who);
                    self.faces.insert(index);
                    gutter.cells(index, who, spoke != previous)
                }
                Some(_) => Vec::new(),
                // Nobody said it, so there is nothing to draw and no face is
                // claimed for transmission: `gutter_rows` falls back to the
                // blank cell on every row of the block.
                None => Vec::new(),
            };
            // D113 ruled "avatar and name, or name alone"; only the first half
            // was ever built, so with `chatAvatars` off — the default — a room
            // page drew every speaker identically and there was no way to tell
            // who said what. The name is the identity when no portrait is: one
            // row per run, in the colour the roster gives that same name.
            //
            // Only a message carrying an *explicit* speaker takes one — a
            // room's or an agent's page, where the participants cannot be
            // derived. Main's own flow leaves `speaker` unset and reads it back
            // out of the text (`speaker_of`), so it renders byte-identically,
            // and the user keeps the bubble that already says who they are.
            let name_row = match (&self.conv.messages[i].speaker, role) {
                (Some(who), Role::Assistant) if !gutter.faces && spoke != previous => {
                    let color = pal.avatars[gutter.index_for(who) % pal.avatars.len()];
                    Some(Row::new(Line::styled(
                        format!("@{who}"),
                        SegStyle::fg(color).bold(),
                    )))
                }
                _ => None,
            };
            // Consecutive arrivals read as one batch (the user's ruling, with
            // the tool groups' own argument): three dispatches completing are
            // one event to the reader, not three blocks with a blank row each.
            // Only the `●` notices coalesce, and only with each other; the `⚠`
            // alert keeps its own block, because bad news does not queue
            // politely. The decision reads nothing but the previous message's
            // settled text, so a block renders the same on every frame
            // (write-once).
            let arrival = |text: &str| crate::tui::bufferview::is_agent_notice(text);
            let in_streak = i > 0
                && role == Role::User
                && arrival(&self.conv.messages[i].text)
                && self.conv.messages[i - 1].role == Role::User
                && arrival(&self.conv.messages[i - 1].text);
            let block = {
                let mut col = Vec::new();
                if !in_streak {
                    col.push(El::Blank);
                }
                if let Some(row) = name_row {
                    col.push(El::Rows(vec![row]));
                }
                col.push(El::gutter(cells, gutter.blank(), body));
                El::col(col)
            };
            blocks.push(Block::settled(block, settled));
        }
        if let Some(ask) = self.ask_el(&theme) {
            blocks.push(Block::live(ask));
        }
        // Slash command output (/help /status /compact etc.): transient hints — rendered after messages and
        // above the input, **never settled or flushed**, auto-dismissed after the tick timeout (SLASH_OUTPUT_TTL).
        //
        // On every page since D135. The three tiers are the console's answer to
        // the console's own commands, and those run wherever the screen is —
        // suppressing them here was the display half of the split `submit`, and
        // it would now swallow the answer to a command the user just ran.
        if !self.slash_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.text)))
                    .collect(),
            )));
        }
        // Error/usage rows (G12/G13): longer TTL, error color, clear on the next input.
        if !self.slash_error_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_error_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.error)))
                    .collect(),
            )));
        }
        // Informational output (/help /status …): persists until the next
        // input/Esc; never settles into scrollback.
        if !self.slash_info_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_info_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.text)))
                    .collect(),
            )));
        }

        self.doc = crate::tui::statics::layout(blocks);
    }

    /// Welcome-card block. It settles at birth but stays in the live doc
    /// (banner breathing, re-wrap on resize) until it crosses the window top.
    fn welcome_el(&self, width: usize, theme: &Theme) -> El {
        // New-version banner (update-banner): breathing color inside the window; outside / no banner → resting rest or None.
        let banner = self.update_banner.as_deref().map(|v| {
            let frame = self.update_banner_frame().unwrap_or(UPDATE_BANNER_FRAMES);
            (v, self.motion.breath(theme, frame))
        });
        let provider = self.provider();
        El::Rows(welcome_card_rows(
            theme,
            &self.model(),
            self.permission_mode_label(),
            &self.cwd,
            width,
            banner,
            !self.session.client.is_configured(&provider),
        ))
    }

    /// Assistant message: markdown text and activities interleaved in model
    /// output order; collapse groups fold runs of read/search tools. `settled`
    /// mirrors the old `message_settled(i)` (prefix-monotone flag).
    /// The portrait each of this message's activities wears, resolved in one pass
    /// before the rows are built (the row loop holds a read borrow of `messages`,
    /// and recording a face needs a write).
    ///
    /// Only a subagent watch row gets one, and only where the terminal can place
    /// images: the face is what buys the `⎿` connector's place, so a chip skin —
    /// which has no face to spend — keeps `◉` and the connector exactly as before.
    /// With `experimental.chatAvatars` off the transcript wears no faces at all,
    /// which lands in the same place as a terminal that cannot draw them.
    fn watch_portraits(
        &mut self,
        i: usize,
        pal: &crate::tui::avatar::Palette,
        grouped: &[Option<Vec<usize>>],
    ) -> Vec<Option<Portrait>> {
        if !self.chat_avatars || self.image_cap.is_none() {
            return Vec::new();
        }
        let named: Vec<Option<String>> = self.conv.messages[i]
            .activities
            .iter()
            .enumerate()
            .map(|(idx, act)| match &act.kind {
                // A row inside a grouped dispatch draws no face — the stem and
                // the name in its identity colour are what CC's tree carries —
                // so no picture is claimed for one either: `Chat::faces` is what
                // the transmit sweep sends, and sending an image nothing draws
                // is paying for a hole that never appears.
                ActivityKind::Watch(w)
                    if w.kind == crate::watch::WatchKind::Agent
                        && grouped.get(idx).and_then(Option::as_ref).is_none() =>
                {
                    let name = crate::tui::activities::watch_instance(&w.label).trim();
                    (!name.is_empty()).then(|| name.to_string())
                }
                _ => None,
            })
            .collect();
        named
            .into_iter()
            .map(|name| {
                let name = name?;
                let index = self
                    .faces_pinned
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| avatar::index_of(&name));
                self.faces.insert(index);
                Some(Portrait {
                    top: crate::tui::avatar::gutter_cell(index, &name, 0, true, pal),
                    bottom: crate::tui::avatar::gutter_cell(index, &name, 1, true, pal),
                })
            })
            .collect()
    }

    /// The runs of adjacent dispatch rows one round opened, as a per-activity
    /// map: `Some(members)` on the first of a run of two or more,
    /// `Some(vec![])` on the rest of it, `None` everywhere else.
    ///
    /// CC groups the `Agent` tool calls of *one assistant message* into a
    /// single block (`renderGroupedAgentToolUse`, `tools/AgentTool/UI.tsx:649`);
    /// the analogue here is a run of watch rows sharing an insert point, which
    /// is what "the model made these calls in one round" looks like once the
    /// rows are hung off the message's text.
    ///
    /// **A group that anybody has opened is not a group.** The folded form has
    /// one status row per agent and no room for content, so an expanded member
    /// falls back to the individual rows — which is also what the `ctrl+o`
    /// transcript gets, since it opens every activity before it builds.
    fn dispatch_groups(&self, i: usize) -> Vec<Option<Vec<usize>>> {
        let msg = &self.conv.messages[i];
        let is_dispatch = |idx: usize| -> bool {
            msg.group_of.get(idx).copied().flatten().is_none()
                && matches!(&msg.activities[idx].kind,
                    ActivityKind::Watch(w) if w.kind == crate::watch::WatchKind::Agent)
        };
        let mut out: Vec<Option<Vec<usize>>> = vec![None; msg.activities.len()];
        let mut start = 0usize;
        while start < msg.activities.len() {
            if !is_dispatch(start) {
                start += 1;
                continue;
            }
            let at = msg.insert_points.get(start).copied();
            let mut end = start + 1;
            while end < msg.activities.len()
                && is_dispatch(end)
                && msg.insert_points.get(end).copied() == at
            {
                end += 1;
            }
            let members: Vec<usize> = (start..end).collect();
            if members.len() > 1 && !members.iter().any(|&m| msg.activities[m].expanded) {
                out[start] = Some(members);
                for slot in out.iter_mut().take(end).skip(start + 1) {
                    *slot = Some(Vec::new());
                }
            }
            start = end;
        }
        out
    }

    /// One block for the several agents a round dispatched — CC's grouped tree
    /// (`tools/AgentTool/UI.tsx:740-762` for the header,
    /// `components/AgentProgressLine.tsx` for the rows).
    ///
    /// ```text
    /// ⏺ Running 2 agents…
    ///    ├─ @scout: fix the parser · 4 tool uses · 2.1k tokens
    ///    │  ⎿  ⏺ Read(src/lexer.rs)
    ///    └─ @zoe: run the tests · 7 tool uses · 3.1k tokens
    ///       ⎿  Done
    /// ```
    fn dispatch_group_el(&self, i: usize, members: &[usize], theme: &Theme) -> El {
        let colors: Vec<Color> = members
            .iter()
            .map(|&m| match &self.conv.messages[i].activities[m].kind {
                ActivityKind::Watch(w) => {
                    self.identity_color(crate::tui::activities::watch_instance(&w.label))
                }
                _ => theme.text,
            })
            .collect();
        let msg = &self.conv.messages[i];
        let calls: Vec<&crate::tui::activities::WatchCall> = members
            .iter()
            .filter_map(|&m| match &msg.activities[m].kind {
                ActivityKind::Watch(w) => Some(w),
                _ => None,
            })
            .collect();
        let unresolved = calls.iter().any(|w| !w.status.is_terminal());
        // CC's two headings, and its `agents` plural: `commonType` only fills in
        // when every spawn is of one custom type, which named instances never are.
        let mut header = Line::styled(
            "⏺ ",
            if unresolved {
                theme.dim()
            } else {
                theme.tool_done()
            },
        );
        header.push_styled(
            if unresolved {
                format!("Running {} agents…", calls.len())
            } else {
                format!("{} agents finished", calls.len())
            },
            SegStyle::fg(theme.text),
        );
        let mut rows = vec![Row::new(header)];
        let mut clicks: Vec<LocalClick> = Vec::new();
        for (n, w) in calls.iter().enumerate() {
            let last = n + 1 == calls.len();
            let start = rows.len();
            let mut line = Line::styled(if last { "   └─ " } else { "   ├─ " }, theme.dim());
            line.push_styled(
                format!("@{}", crate::tui::activities::watch_instance(&w.label)),
                SegStyle::fg(colors[n]),
            );
            let description = crate::tui::activities::watch_description(&w.label);
            if !description.is_empty() {
                line.push_styled(format!(": {description}"), theme.dim());
            }
            let stats = w.run_stats.unwrap_or_default();
            line.push_styled(
                crate::tui::tree::stats_label(stats.tool_uses, stats.tokens),
                theme.dim(),
            );
            rows.push(Row::new(line));
            // CC's status text for a row inside the group is one word once the
            // agent is resolved (`AgentProgressLine` `getStatusText`): the full
            // `Done (…)` belongs to the ungrouped row, which is where the run's
            // cost is the point rather than the fact that it ended.
            let (status, style) = match w.status {
                WatchState::Done => ("Done".to_string(), theme.dim()),
                WatchState::Failed => (
                    w.detail.clone().unwrap_or_else(|| "Failed".to_string()),
                    theme.tool_error(),
                ),
                WatchState::Cancelled => ("Cancelled".to_string(), theme.dim()),
                _ => (
                    w.progress
                        .last()
                        .cloned()
                        .unwrap_or_else(|| crate::tui::activities::INITIALIZING.to_string()),
                    theme.dim(),
                ),
            };
            let stem = if last { "      ⎿  " } else { "   │  ⎿  " };
            rows.push(Row::new(Line::styled(format!("{stem}{status}"), style)));
            clicks.push(LocalClick {
                start,
                end: rows.len(),
                target: ClickTarget::Activity {
                    message: i,
                    path: vec![members[n]],
                },
            });
        }
        El::Annotated { rows, clicks }
    }

    fn assistant_el(
        &mut self,
        i: usize,
        width: usize,
        theme: &Theme,
        settled: bool,
        pal: &crate::tui::avatar::Palette,
    ) -> El {
        let groups = self.dispatch_groups(i);
        let portraits = self.watch_portraits(i, pal, &groups);
        // CC drops a dispatch's per-tool rows for one condensed line when the
        // window cannot hold them (`tools/AgentTool/UI.tsx:469`): its estimate is
        // `in-progress calls × lines-per-call + buffer`. The arithmetic is CC's;
        // the per-call figure is bingo's own, because a dispatch row here is a
        // header plus at most `PROGRESS_LINES`, not a full tool rendering.
        let running_dispatches = self.conv.messages[i]
            .activities
            .iter()
            .filter(|a| {
                matches!(&a.kind,
                    ActivityKind::Watch(w)
                        if w.kind == crate::watch::WatchKind::Agent && !w.status.is_terminal())
            })
            .count();
        let narrow = running_dispatches > 0
            && self.height
                < running_dispatches * (1 + crate::tui::activities::PROGRESS_LINES)
                    + DISPATCH_BUFFER_LINES;
        // Built before the render closure takes its field borrows, and in index
        // order, so the block lands exactly where the first of its run would.
        let mut group_els: Vec<Option<El>> = groups
            .iter()
            .map(|members| match members {
                Some(members) if !members.is_empty() => {
                    Some(self.dispatch_group_el(i, members, theme))
                }
                _ => None,
            })
            .collect();
        // Thinking completion row (CC SystemTextMessage `✻ Churned for 40s`):
        // rendered at the end of the message (after text and all tools), from the last completed
        // real thinking block (empty placeholder blocks produce no completion row).
        // Only rendered after the turn ends: while running, `✻ Baked for 0.4s` would appear
        // while tools are still running, contradicting the bottom running-status row.
        let show_done_line =
            i == self.conv.messages.len() - 1 && self.conv.stream_msg.is_none() || settled;
        // The `settle` token (D87): the completion row of the turn that just
        // ended carries the accent for one 120ms window. Only the last message
        // can be settling — every earlier one finished long ago.
        let settling = i + 1 == self.conv.messages.len() && self.settling();
        // Built before the render closure takes its mutable borrows: the tail is the
        // same rows wherever the running command's row turns out to be inside this
        // message. Only the streaming message can hold one — the same rule the tool
        // events themselves follow — so every other message pays nothing.
        let bash_tail = if self.conv.stream_msg == Some(i) {
            self.bash_tail_rows(width)
        } else {
            Vec::new()
        };
        // Markdown render closure: borrows only disjoint fields to avoid conflicting with
        // the shared read borrow of `self.conv.messages`.
        let mut render = {
            let processor = &mut self.processor;
            let renderer = &mut self.renderer;
            let cache = &mut self.reply_cache;
            let images = &self.images;
            let images_failed = &self.images_failed;
            let image_cap = self.image_cap;
            let images_version = self.images_version;
            move |reply: &str| -> Vec<Line> {
                if reply.is_empty() {
                    return Vec::new();
                }
                if let Some(lines) = cache.get(reply) {
                    return lines.clone();
                }
                renderer.set_width(width.saturating_sub(2));
                // Image cache version changed → sync the renderer (clears its per-block cache).
                if renderer.images_version() != images_version {
                    renderer.set_images(image_cap, images, images_failed, images_version);
                }
                let doc = processor.process_streaming(reply);
                renderer.render(&doc);
                let lines = renderer.lines().to_vec();
                cache.insert(reply.to_string(), lines.clone());
                lines
            }
        };
        let msg = &self.conv.messages[i];
        let text = &msg.text;
        let char_bounds: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
        let mut rendered_chars = 0usize;
        let mut rendered_bytes = 0usize;
        let mut parts: Vec<El> = Vec::new();
        for (idx, act) in msg.activities.iter().enumerate() {
            let pos_chars = msg
                .insert_points
                .get(idx)
                .copied()
                .unwrap_or(rendered_chars)
                .min(text.chars().count());
            if pos_chars > rendered_chars {
                let seg_end = char_bounds.get(pos_chars).copied().unwrap_or(text.len());
                let reply = render(&text[rendered_bytes..seg_end]);
                parts.push(text_el(theme, reply));
                rendered_chars = pos_chars;
                rendered_bytes = seg_end;
            }
            // A round that dispatched several agents draws one block for all of
            // them (D106), on the first of the run; the rest of the run draws
            // nothing of its own.
            if let Some(members) = groups.get(idx).and_then(Option::as_ref) {
                if !members.is_empty()
                    && let Some(block) = group_els[idx].take()
                {
                    parts.push(block);
                }
                continue;
            }
            let group_idx = msg.group_of.get(idx).copied().flatten();
            let group_collapsed = group_idx.is_some_and(|g| !msg.groups[g].expanded);
            let is_group_head =
                group_idx.is_some_and(|g| msg.groups[g].activities.first() == Some(&idx));
            if group_collapsed && !is_group_head {
                continue;
            }
            if let Some(g) = group_idx
                && !msg.groups[g].expanded
            {
                // Collapse group: a one-line rule summary (`Read 3 files (ctrl+o to expand)`).
                let in_progress = msg.groups[g].active
                    && msg.groups[g].activities.iter().any(|&ai| {
                        matches!(
                            msg.activities.get(ai),
                            Some(a) if matches!(
                                &a.kind,
                                ActivityKind::Tool(t)
                                    if t.status == ToolStatus::Running
                            )
                        )
                    });
                let summary = collapse_summary(&msg.groups[g], in_progress);
                // A failure inside the fold is otherwise invisible: the summary counts the call
                // as if it had worked, and only ctrl+o shows the error row. Say so on the
                // summary line — it matters most for the calls that change something.
                let failed = msg.groups[g]
                    .activities
                    .iter()
                    .filter(|&&ai| {
                        matches!(
                            msg.activities.get(ai),
                            Some(a) if matches!(
                                &a.kind,
                                ActivityKind::Tool(t) if t.status == ToolStatus::Error
                            )
                        )
                    })
                    .count();
                // The group row is a static `⏺ …`: the spinner only lives in the bottom status row.
                let mut line = Line::styled(
                    "⏺ ",
                    if in_progress {
                        theme.dim()
                    } else {
                        theme.tool_done()
                    },
                );
                line.push_styled(summary, SegStyle::fg(theme.text));
                if failed > 0 {
                    line.push_styled(format!(" · {failed} failed"), SegStyle::fg(theme.error));
                }
                line.push_styled(
                    " (ctrl+o to expand)".to_string(),
                    SegStyle::fg(theme.text_secondary),
                );
                parts.push(El::click(
                    ClickTarget::Group {
                        message: i,
                        group: g,
                    },
                    El::Line(line),
                ));
                // Below a running collapse group, show the most recent tool's input (the CC ⎿ row).
                // The hint may be a multi-line bash command: single-line it and truncate by width,
                // otherwise the row balloons into multiple lines and the row model drifts from the canvas.
                // It sits outside the Click wrapper — only the summary row toggles.
                if in_progress && let Some(hint) = &msg.groups[g].last_hint {
                    parts.push(El::Line(Line::styled(
                        one_line(&format!("  ⎿  {hint}"), width),
                        SegStyle::fg(theme.text_secondary),
                    )));
                }
                // The folded command is the one running: its tail belongs under the
                // hint row, which is the only row the fold gives it (D84).
                if in_progress
                    && msg.groups[g]
                        .activities
                        .iter()
                        .any(|&ai| msg.activities.get(ai).is_some_and(is_running_bash))
                {
                    parts.extend(bash_tail.iter().cloned().map(El::Line));
                }
                continue;
            }
            let (lines, local) = layout_activity(
                act,
                &[idx],
                0,
                theme,
                portraits.get(idx).and_then(|p| p.as_ref()),
                narrow,
                &mut |reply: &str| render(reply),
            );
            let activity = El::Annotated {
                rows: lines.into_iter().map(Row::new).collect(),
                clicks: local
                    .into_iter()
                    .map(|range| LocalClick {
                        start: range.start as usize,
                        end: range.end as usize,
                        target: ClickTarget::Activity {
                            message: i,
                            path: range.path,
                        },
                    })
                    .collect(),
            };
            // Expanded group: the group-head tool row doubles as the group summary row — the
            // enclosing Click is emitted first, so clicking it collapses the group back.
            parts.push(if let Some(g) = group_idx {
                El::click(
                    ClickTarget::Group {
                        message: i,
                        group: g,
                    },
                    activity,
                )
            } else {
                activity
            });
            // The command's output so far, under its own row. Outside the Annotated
            // block: these rows are evidence, not a click target, and they are gone
            // by the time the row is worth clicking.
            if is_running_bash(act) {
                parts.extend(bash_tail.iter().cloned().map(El::Line));
            }
        }
        if rendered_bytes < text.len() {
            let reply = render(&text[rendered_bytes..]);
            parts.push(text_el(theme, reply));
        }
        if show_done_line
            && let Some(line) = self.conv.messages[i]
                .activities
                .iter()
                .rev()
                .find_map(|a| match &a.kind {
                    // The line reports a duration, so it needs one. A
                    // rebuilt page has none — no clock is in the history
                    // (D130) — and `✻ Thinking for 0.0s` would be a
                    // measurement nobody took.
                    ActivityKind::Thinking(t)
                        if t.state == ThinkingState::Done && t.timed && !a.content.is_empty() =>
                    {
                        Some(crate::tui::activities::thinking_completion_line(
                            t, theme, settling,
                        ))
                    }
                    _ => None,
                })
        {
            parts.push(El::Line(line));
        }
        // Send time beside the reply's opening row (D93), and only once the turn
        // has finished — a clock arriving mid-stream would read as an ending.
        let time = crate::tui::buffer::stamp(self.conv.messages[i].at);
        let mut el = El::Col(parts);
        if show_done_line {
            hang_stamp(&mut el, &time, width, theme);
        }
        el
    }

    /// Resets the flush cursor: after the message set is replaced wholesale (/clear, /resume), segment numbers
    /// are invalid, so the doc rebuilds from the welcome card (new content flushes into scrollback again).
    pub(crate) fn reset_flushed(&mut self) {
        self.flushed_segments = 0;
        self.tail_start = 0;
        self.mark_base = 0;
        self.dirty = true;
    }

    /// After flushing `doc.rows[tail_start..settled]`, advance the cursor: the next rebuild skips
    /// those segments and the current doc's tail start moves up (the canvas stops drawing them before a rebuild).
    // Production advances partially by checkpoints (lazy flush); full advance stays as a test-facing primitive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn advance_flushed(&mut self) {
        if let Some(mark) = self.doc.settled_marks.last().copied() {
            self.advance_flushed_upto(mark);
        }
    }

    /// After flushing `doc.rows[tail_start..mark.row_end]`, advance the cursor to that checkpoint.
    /// Callable multiple times within one build (`mark_base` absorbs the build-internal accumulators,
    /// preventing double-counting); safe across width-change re-layouts — segment counts are row-number independent.
    pub fn advance_flushed_upto(&mut self, mark: SettledMark) {
        self.flushed_segments += mark.segments.saturating_sub(self.mark_base);
        self.mark_base = mark.segments;
        self.tail_start = mark.row_end;
    }

    /// After a resize the window can hold more: pull the most recently flushed content back into the live doc
    /// to refill it. Old copies in scrollback cannot be physically retracted — accept seeing a duplicate
    /// at the old width when scrolling up (an explicitly accepted trade-off, see research.md D27). Rehydration is
    /// purely bookkeeping (writes nothing to the terminal), bounded by "no more than `doc_budget` rows"; beyond that it rolls back,
    /// guaranteeing no conflict with lazy flushing (after rehydration no settled segment crosses the window top).
    pub fn rehydrate(&mut self, width: usize, doc_budget: usize) {
        loop {
            if self.flushed_segments == 0 {
                break;
            }
            if self.build_rows(width).rows.len() >= doc_budget {
                break;
            }
            self.flushed_segments -= 1;
            if self.build_rows(width).rows.len() > doc_budget {
                self.flushed_segments += 1;
                break;
            }
        }
        self.dirty = true;
    }
}

/// Columns a send stamp holds clear of the message it sits beside.
const STAMP_GAP: usize = 2;

/// Hang a message's send stamp on its own first row, right-aligned (D93).
///
/// The stamp used to be a row of its own under the body: a whole terminal line
/// spent on five characters, and — repeated down a transcript — a column of
/// clocks that read louder than the messages between them. Beside the opening
/// row it is what it always was, furniture, and it costs nothing.
///
/// Where the row is too narrow to hold body and stamp [`STAMP_GAP`] apart, the
/// stamp is simply not drawn. Content wins: no message is wrapped or truncated
/// to make room for a clock.
fn hang_stamp(el: &mut El, time: &str, width: usize, theme: &Theme) {
    if time.is_empty() {
        return;
    }
    let Some((line, padding_right)) = el.first_content_line_mut() else {
        return;
    };
    // A bubble row reserves its rightmost column and the renderer clips to it,
    // so the stamp aligns inside that edge rather than the terminal's.
    crate::tui::line::push_right(
        line,
        time,
        theme.muted(),
        width.saturating_sub(padding_right),
        STAMP_GAP,
    );
}

pub(crate) fn manager_box(rows: Vec<Row>, width: usize, theme: &Theme) -> Vec<Row> {
    let inner = width.saturating_sub(4).max(1);
    let border = "─".repeat(inner);
    // The frame is furniture; what it frames is not.
    let frame = theme.muted();
    let mut out = Vec::with_capacity(rows.len() + 2);
    out.push(Row::new(Line::styled(format!("╭{border}╮"), frame)));
    for row in rows {
        let mut line = Line::styled("│ ", frame);
        let mut used = 0usize;
        for seg in row.line.segs {
            if used >= inner.saturating_sub(2) {
                break;
            }
            let remaining = inner.saturating_sub(2 + used);
            let text = one_line(&seg.text, remaining.max(1));
            used += text_width(&text);
            line.push_styled(text, seg.style);
        }
        line.push_styled(
            format!("{} │", " ".repeat(inner.saturating_sub(used + 2))),
            frame,
        );
        let mut boxed = Row::new(line);
        boxed.bg = row.bg;
        boxed.padding_right = row.padding_right;
        out.push(boxed);
    }
    out.push(Row::new(Line::styled(format!("╰{border}╯"), frame)));
    out
}

fn text_el(theme: &Theme, reply: Vec<Line>) -> El {
    El::Rows(text_rows(theme, reply))
}

/// Welcome card body (CC WelcomeBox): a starred greeting, the two commands
/// worth knowing, the cwd, and a dim identity line. `bingo` stays `bingo` —
/// this is homage, not impersonation.
///
/// The new-version banner row (update-banner spec v1.1): sits directly above the version-identity row, one blank
/// row from cwd; three segments (static inactive + version/command in breathing color, command bold),
/// breathing only affects the banner's two keyword segments; every other welcome-card element stays static.
fn welcome_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
    banner: Option<(&str, Color)>,
    unconfigured: bool,
) -> Vec<Line> {
    let mut rows = Vec::new();
    let mut greeting = Line::styled(" ✻ ", SegStyle::fg(theme.claude));
    greeting.push_styled("Welcome back!", theme.text());
    rows.push(greeting);
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line("   /help for help · /status for your current setup", width),
        theme.dim(),
    ));
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line(&format!("   cwd: {cwd}"), width),
        theme.dim(),
    ));
    // Onboarding: with no usable credentials, the card says what to do next —
    // the login command lives in here, so the door must open before the key.
    if unconfigured {
        rows.push(Line::empty());
        rows.push(Line::styled(
            one_line(
                "   ⚠ no credentials configured: /provider login codex (ChatGPT subscription) or write apiKey in ~/.config/bingo/settings.json",
                width,
            ),
            SegStyle::fg(theme.warning),
        ));
    }
    // New-version banner row (update-banner spec §1.1): directly above the version-identity row, one blank row from cwd.
    if let Some((v, color)) = banner
        && let Some((pre, ver, mid, cmd)) = banner_segments(v, width)
    {
        rows.push(Line::empty());
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let mut line = Line::styled(&pre, theme.dim());
        if no_color {
            // Monochrome / NO_COLOR fallback: a static bold row (spec §2.5).
            line.push_styled(ver, theme.dim());
            line.push_styled(mid, theme.dim());
            line.push_styled(cmd, theme.dim().bold());
        } else {
            line.push_styled(ver, SegStyle::fg(color));
            line.push_styled(mid, theme.dim());
            line.push_styled(cmd, SegStyle::fg(color).bold());
        }
        rows.push(line);
    }
    rows.push(Line::styled(
        one_line(
            &format!("   bingo v{} · {model} · {mode}", env!("CARGO_PKG_VERSION")),
            width,
        ),
        theme.dim(),
    ));
    rows
}

/// Welcome card rows (with the ╭╮ border), part of the scrollable content.
/// `banner` = the new-version hint (version + current breathing color); None = no banner row.
pub(crate) fn welcome_card_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
    banner: Option<(&str, Color)>,
    unconfigured: bool,
) -> Vec<Row> {
    let gray = theme.muted();
    let inner_w = width.saturating_sub(2);
    let mut rows = vec![Row::new(Line::styled(
        format!("╭{}╮", "─".repeat(inner_w)),
        gray,
    ))];
    for line in welcome_rows(theme, model, mode, cwd, inner_w, banner, unconfigured) {
        let mut styled = Line::styled("│", gray);
        let pad = inner_w.saturating_sub(text_width(&line.plain_text()));
        styled.segs.extend(line.segs);
        styled.push_styled(" ".repeat(pad), gray);
        styled.push_styled("│", gray);
        rows.push(Row::new(styled));
    }
    rows.push(Row::new(Line::styled(
        format!("╰{}╯", "─".repeat(inner_w)),
        gray,
    )));
    rows
}

/// Update-banner breathing window: 270 frames = 9s (3 breaths; each cycle is 90 frames = 3.0s @30fps).
/// After the window it rests at the rest color and the banner stays (update-banner spec §2.3).
pub const UPDATE_BANNER_FRAMES: u64 = 270;

/// Banner truncation chain (update-banner spec §1.3, pure and testable): returns
/// (pre, ver, mid, cmd) — the static segment and the two breathing segments are separate so the render layer can color them.
///
/// | inner width | shown as |
/// |---|---|
/// | ≥50 (or the full line fits) | `   New version vX.Y.Z available — run bingo update` |
/// | ≥43 | `   New version vX.Y.Z — run bingo update` |
/// | ≥15 | `   bingo update` (the command alone, the minimal action entry) |
/// | <15 | None (banner hidden) |
///
/// At every tier `bingo update` stays visible, unwrapped, and inside the card.
pub fn banner_segments(v: &str, width: usize) -> Option<(String, String, String, String)> {
    const PRE: &str = "   New version ";
    const MID_FULL: &str = " available — run ";
    const MID_SHORT: &str = " — run ";
    const CMD: &str = "bingo update";
    let ver = format!("v{v}");
    let full_len = text_width(PRE) + text_width(&ver) + text_width(MID_FULL) + text_width(CMD);
    if width >= 50 || full_len <= width {
        return Some((PRE.to_string(), ver, MID_FULL.to_string(), CMD.to_string()));
    }
    if width >= 43 {
        return Some((PRE.to_string(), ver, MID_SHORT.to_string(), CMD.to_string()));
    }
    if width >= 15 {
        return Some((
            String::new(),
            String::new(),
            "   ".to_string(),
            CMD.to_string(),
        ));
    }
    None
}

/// The banner's full text (the string form of `banner_segments`; the pure function the spec names, used by test assertions).
#[cfg_attr(not(test), allow(dead_code))]
pub fn banner_line(v: &str, width: usize) -> Option<String> {
    banner_segments(v, width).map(|(pre, ver, mid, cmd)| format!("{pre}{ver}{mid}{cmd}"))
}

/// Whether this activity is the foreground shell command the live tail belongs
/// to. Bash is never concurrency-safe, so Phase 2 runs it alone: at most one
/// activity can answer yes at a time, which is why one tail slot is enough.
fn is_running_bash(act: &Activity) -> bool {
    matches!(&act.kind, ActivityKind::Tool(t) if t.status == ToolStatus::Running && t.name == "Bash")
}

/// Whether this activity is a dispatch row whose run is still going — the rows
/// D106 keeps sampling from the registry.
fn is_running_dispatch(act: &Activity) -> bool {
    matches!(&act.kind,
        ActivityKind::Watch(w)
            if w.kind == crate::watch::WatchKind::Agent && !w.status.is_terminal())
}
