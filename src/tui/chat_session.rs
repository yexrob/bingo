//! Session commands and the session picker (split out of chat.rs, D91):
//! `/rename`, `/resume`, `/gc` and `/share`, the transcript switch behind them,
//! and the `/resume` selector model. Owns no state; `impl super::Chat`.
//!
//! `/compact` joined them in D135, for the reason the rest are here and for
//! one of its own: it is the command that *rewrites* a session's record, and
//! it is now the one command that asks which page the reader is on before it
//! decides whose record that is.

use super::*;

/// /resume selector option cap (devex DX: sessions can be many; truncate to the latest N + a note row).
pub const RESUME_PICKER_MAX: usize = 20;

/// `/resume` session selector (picker-model.md commit C): dynamic single-level (disk snapshot),
/// Enter switches the session; label=display name, value=session name; confirmation takes the snapshot by the selected index.
#[derive(Clone)]
pub struct ResumeMenu {
    /// Browsed index (❯): moves with ↑↓/1-20, applied only on Enter.
    pub selected: usize,
    /// The current session's position in the list (●; None when absent or unset).
    pub current: Option<usize>,
    /// Session-list snapshot (same order as items; confirmation picks the Transcript by selected).
    pub transcripts: Vec<crate::transcript::Transcript>,
    /// The list was truncated (past RESUME_PICKER_MAX) → render a note row.
    pub truncated: bool,
}

impl ResumeMenu {
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.transcripts
                .iter()
                .map(|t| {
                    let count = t.load_messages().unwrap_or_default().len();
                    crate::tui::picker::PickerItem::new(
                        t.name(),
                        t.name(),
                        format!("{count} messages"),
                    )
                })
                .collect(),
            self.selected,
            self.current,
        )
    }

    /// Scene key configuration: no s (switching sessions is the intent), number jump 1-20.
    pub fn keys() -> crate::tui::picker::PickerKeys {
        crate::tui::picker::PickerKeys {
            session_only: false,
            number_jump: true,
        }
    }
}

impl super::Chat {
    pub(super) fn slash_rename(&mut self, arg: &str) {
        let done = crate::engine::actions::rename_session(&self.session, arg);
        for warning in done.warnings {
            self.push_warning(warning);
        }
        self.say(done.said);
    }

    /// `/resume [name or keyword]`: no argument opens the session selector (picker-model.md commit C,
    /// the same picker as CC's /resume); an argument takes the fast path (name/keyword match, kept as-is).
    pub(super) fn slash_resume(&mut self, arg: &str) {
        let home = self.session.home.clone();
        let transcripts = match crate::transcript::list(&home) {
            Ok(t) => t,
            Err(e) => {
                self.push_slash_error(format!("cannot read the session list: {e}"));
                return;
            }
        };
        if arg.is_empty() {
            if transcripts.is_empty() {
                self.push_slash_output("no past sessions.".to_string());
                return;
            }
            self.open_resume_menu(transcripts);
            return;
        }
        self.switch_transcript(transcripts.iter().find(|t| t.name().contains(arg)), arg);
    }

    /// Fast-path switch (argument /resume): a hit switches, a miss errors.
    fn switch_transcript(&mut self, found: Option<&crate::transcript::Transcript>, arg: &str) {
        let Some(found) = found else {
            self.push_slash_error(format!("no session contains '{arg}'."));
            return;
        };
        if let Err(error) = found.activate() {
            self.push_slash_error(format!("cannot resume session: {error}"));
            return;
        }
        let count = found.load_messages().unwrap_or_default().len();
        let _ = self.session.runtime.transcript_tx.send(Some(found.clone()));
        self.rebind_tasks_to_transcript(Some(found));
        self.attach_share_to_transcript(Some(found));
        self.conv.messages.clear();
        self.slash_lines.clear();
        self.reset_flushed();
        self.refresh_context_usage_from_transcript();
        self.push_slash_output(format!(
            "✓ switched to session {} ({count} messages); the next reply uses its history.",
            found.name()
        ));
    }

    /// Open the `/resume` selector: truncate the disk snapshot to the latest RESUME_PICKER_MAX,
    /// ● marks the current session (when in the list), other menus close exclusively.
    fn open_resume_menu(&mut self, mut transcripts: Vec<crate::transcript::Transcript>) {
        let truncated = transcripts.len() > RESUME_PICKER_MAX;
        transcripts.truncate(RESUME_PICKER_MAX);
        let current = self.session.runtime.transcript.borrow().clone();
        let current = current
            .as_ref()
            .and_then(|cur| transcripts.iter().position(|t| t.path() == cur.path()));
        let menu = ResumeMenu {
            selected: current.unwrap_or(0),
            current,
            transcripts,
            truncated,
        };
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.resume_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Resume menu keys: ↑↓/1-N move (delegated to the PickerModel core),
    /// Enter switches the session (by selected index into the snapshot), Esc exits.
    pub(crate) fn resume_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.resume_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            // Direct jump: 1..=min(len, 9) (past 9 items the number jump only covers the first 9).
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Enter => {
                // The confirm action takes the snapshot by the selected index (same order as items; the value≠label test anchor).
                let Some(t) = menu.transcripts.get(menu.selected).cloned() else {
                    return false;
                };
                if let Err(error) = t.activate() {
                    self.resume_menu = None;
                    self.push_slash_error(format!("cannot resume session: {error}"));
                    return true;
                }
                let name = t.name();
                let count = t.load_messages().unwrap_or_default().len();
                self.resume_menu = None;
                let _ = self.session.runtime.transcript_tx.send(Some(t.clone()));
                self.rebind_tasks_to_transcript(Some(&t));
                self.attach_share_to_transcript(Some(&t));
                self.conv.messages.clear();
                self.slash_lines.clear();
                self.reset_flushed();
                self.refresh_context_usage_from_transcript();
                self.push_slash_output(format!(
                    "✓ switched to session {name} ({count} messages); the next reply uses its history."
                ));
                true
            }
            KeyCode::Esc => {
                self.resume_menu = None;
                true
            }
            _ => false,
        }
    }

    pub(super) fn slash_gc(&mut self) {
        if self.conv.busy {
            self.push_slash_error(format!(
                "[error] code={} msg=cannot clean session data mid-turn (press Esc to interrupt, then retry)",
                crate::error::SLASH_ERROR_BAD_ARGUMENT
            ));
            return;
        }
        let home = self.session.home.clone();
        let protected = self
            .session
            .runtime
            .transcript
            .borrow()
            .as_ref()
            .map(|transcript| transcript.path().to_path_buf());
        self.pin_panel("gc", vec!["⏳ cleaning session data…".to_string()]);
        let result = crate::storage::cleanup(&home, protected.as_deref());
        self.unpin_panel("gc");
        match result {
            Ok(report) => self.push_slash_info(format!("✓ {}", report.summary())),
            Err(error) => {
                self.last_error = Some(ErrorState {
                    code: crate::error::map_error(&error).to_string(),
                    msg: format!(
                        "session storage cleanup failed: {error}; check disk permissions and retry /gc"
                    ),
                    level: crate::error::ErrorLevel::Page,
                    context: crate::error::ErrorContext::ShortSync,
                });
                self.dirty = true;
            }
        }
    }

    /// `/share` exports locally by default. Publishing a public link requires the
    /// explicit `--public` opt-in; the warning is presented before bytes leave the machine.
    ///
    /// What the flags say is the registry's reading, handed in. The handler used
    /// to re-split the same line the registry had already parsed — one grammar
    /// with two readers, which is how a flag comes to mean two things.
    pub(super) fn slash_share(&mut self, public: bool, open: bool) {
        let export = match crate::engine::actions::prepare_share(&self.session, None) {
            Ok(export) => export,
            Err(said) => return self.say(said),
        };
        for note in export.notes.clone() {
            self.say(note);
        }
        // Local export is the safe default; `--open` only opens the generated file.
        if !public {
            let said = crate::engine::actions::export_share(&export, open);
            self.say(said);
            return;
        }
        // Public publishing is asynchronous so the TUI event loop remains responsive.
        let events = self.events.clone();
        self.pin_panel(
            "share",
            vec![
                "⚠ about to publish publicly: anyone can access the full conversation and tool outputs, which may contain sensitive information."
                    .to_string(),
                "⏳ publishing the share page…".to_string(),
            ],
        );
        tokio::spawn(async move {
            let said = crate::engine::actions::publish_share(export, open).await;
            events.send(UiEvent::Unpin {
                id: "share".to_string(),
            });
            events.send(super::said_event(said));
        });
    }

    /// `/compact` is the one command that follows the page instead of the
    /// console (D135, the user's ruling).
    ///
    /// Every other slash command is a console setting — the model, the theme,
    /// the working directory — so acting on the console is acting on what the
    /// user meant, whatever page they happen to be reading. Compaction is not
    /// a setting: it rewrites a context, and rewriting the wrong one destroys
    /// work that cannot be got back. `shift+tab` set the precedent by cycling
    /// the *viewed* agent's permission mode, and this is the one command where
    /// the precedent is worth its cost.
    pub(super) fn slash_compact(&mut self, on: &crate::ui::ConvKey) {
        use crate::engine::actions;
        let plan = match actions::plan_compaction(&self.session, on) {
            Ok(plan) => plan,
            Err(said) => return self.say(said),
        };
        // Keyed by instance: the console's own compaction uses the bare id, and
        // one pin for both meant whichever finished first unpinned the other's
        // progress line — the silence the pin exists to prevent (D135a).
        let pin = match &plan.instance {
            Some(name) => format!("compact:@{name}"),
            None => "compact".to_string(),
        };
        self.pin_panel(&pin, vec![plan.waiting.clone()]);
        let session = self.session.clone();
        // The tiers are the console's and answer on whatever page is up; the
        // window figure is the compacted context's and belongs to that page's
        // own footer, so it travels on a sink bound to it.
        let console = self.events.clone();
        let page = self.events.bound_to(on.clone());
        tokio::spawn(async move {
            let done = actions::compact(session, plan).await;
            if let Some(usage) = done.usage {
                page.send(UiEvent::ContextUsage(usage));
            }
            console.send(UiEvent::Unpin { id: pin });
            console.send(super::said_event(done.said));
        });
    }

    /// One line of an action's own report, in the tier it asked for.
    pub(super) fn say(&mut self, said: crate::engine::actions::Said) {
        use crate::engine::actions::Tier;
        match said.tier {
            Tier::Error => self.push_slash_error(said.text),
            Tier::Info => self.push_slash_info(said.text),
            Tier::Output => self.push_slash_output(said.text),
        }
    }
}
