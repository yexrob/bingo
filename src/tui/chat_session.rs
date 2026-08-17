//! Session commands and the session picker (split out of chat.rs, D91):
//! `/rename`, `/resume`, `/gc` and `/share`, the transcript switch behind them,
//! and the `/resume` selector model. Owns no state; `impl super::Chat`.

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

/// `/share` flag parser (`--public` / `--open`).
pub(crate) fn parse_share_arg(arg: &str, flag: &str) -> bool {
    arg.split_whitespace().any(|t| t == flag)
}

impl super::Chat {
    pub(super) fn slash_rename(&mut self, arg: &str) {
        let Some(t) = self.session.runtime.transcript.borrow().clone() else {
            self.push_slash_error("this session has no transcript; cannot rename.".to_string());
            return;
        };
        let old_name = t.name();
        match t.rename(arg) {
            Ok(new_t) => {
                let name = new_t.name();
                if let Err(error) = self.session.tasks.rename_key(&old_name, &name) {
                    self.push_warning(format!(
                        "task data could not follow the renamed session ({error}); tasks remain under the previous session name"
                    ));
                }
                if let Err(error) =
                    crate::share::rename_session_sidecars(&self.session.home, &old_name, &name)
                {
                    self.push_warning(format!(
                        "share data could not follow the renamed session ({error}); export may omit agent/channel history"
                    ));
                }
                let _ = self.session.runtime.transcript_tx.send(Some(new_t.clone()));
                self.attach_share_to_transcript(Some(&new_t));
                self.push_slash_output(format!("✓ session renamed: {name}"));
            }
            Err(e) => self.push_slash_error(format!("rename failed: {e}")),
        }
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
                    code: crate::error::map_error(&error),
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
    pub(super) fn slash_share(&mut self, arg: &str) {
        let public = parse_share_arg(arg, "--public");
        let open = parse_share_arg(arg, "--open");
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            self.push_slash_output("no session to export yet (the new session has not been persisted; send a message first).".to_string());
            return;
        };
        // Human-facing export: the full canonical conversation, not the
        // compacted projection the model sees.
        let messages = match transcript.load_canonical() {
            Ok(m) => m,
            Err(e) => {
                self.push_slash_error(format!("failed to read the session: {e}"));
                return;
            }
        };
        let stem = transcript.name();
        let share_path = crate::share::shares_dir(&self.session.home).join(format!("{stem}.json"));
        let doc = match crate::share::ShareStore::load_or_create(&share_path) {
            Ok(store) => store.snapshot(),
            Err(e) => {
                self.push_slash_error(format!(
                    "cannot read the share document ({e}); exporting the conversation view only."
                ));
                crate::share::ShareDoc::new(stem.clone())
            }
        };
        // Legacy-session fallback: without a share document, derive Team/DM/channel data from the main transcript.
        let doc = if doc.agents.is_empty() && doc.channels.is_empty() {
            crate::share::derive_share_doc(&stem, &messages)
        } else {
            doc
        };
        let html = crate::share_html::render(&doc, &messages);
        let out = std::path::PathBuf::from(&self.cwd).join(format!("{stem}.html"));

        // Local export is the safe default; `--open` only opens the generated file.
        if !public {
            let overwritten = out.exists();
            if let Err(e) = crate::share::write_html_atomic(&out, &html) {
                self.push_slash_error(format!("write failed: {e}"));
                return;
            }
            let mut lines = vec![format!(
                "✓ exported: {}{}",
                out.display(),
                if overwritten { " (overwritten)" } else { "" }
            )];
            if open {
                match crate::share::open_in_browser(&out.display().to_string()) {
                    Ok(_) => lines.push("opened in the browser.".to_string()),
                    Err(e) => lines.push(format!("cannot open the browser: {e}")),
                }
            }
            lines.push(
                "note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing."
                    .to_string(),
            );
            self.push_slash_info(lines.join("\n"));
            return;
        }

        // Public publishing is asynchronous so the TUI event loop remains responsive.
        // The runtime settings snapshot is authoritative for the configured share service.
        let base = self
            .session
            .settings
            .share
            .base_url
            .clone()
            .unwrap_or_else(|| crate::share::DEFAULT_SHARE_BASE.to_string());
        let id = crate::share::share_id(&stem);
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
            let unpin = || {
                events.send(UiEvent::Unpin {
                    id: "share".to_string(),
                });
            };
            match crate::share::upload_share(&base, &id, &html).await {
                Ok(url) => {
                    let mut lines = vec![format!("✓ published: {url}")];
                    if open {
                        match crate::share::open_in_browser(&url) {
                            Ok(_) => lines.push("opened in the browser.".to_string()),
                            Err(e) => lines.push(format!("cannot open the browser: {e}")),
                        }
                    }
                    unpin();
                    // The URL must survive long enough to copy — info tier.
                    events.send(UiEvent::SlashInfo(lines.join("\n")));
                }
                Err(e) => {
                    // Upload failure falls back to a local file + a notice (consistent with the bingo share subcommand).
                    let mut lines = vec![format!(
                        "upload failed ({e}); falling back to a local file."
                    )];
                    let overwritten = out.exists();
                    match crate::share::write_html_atomic(&out, &html) {
                        Ok(()) => lines.push(format!(
                            "✓ exported: {}{}",
                            out.display(),
                            if overwritten { " (overwritten)" } else { "" }
                        )),
                        Err(write_err) => lines.push(format!("write failed: {write_err}")),
                    }
                    if open && crate::share::open_in_browser(&out.display().to_string()).is_ok() {
                        lines.push("opened in the browser.".to_string());
                    }
                    lines.push(
                        "note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing."
                            .to_string(),
                    );
                    unpin();
                    events.send(UiEvent::SlashError(lines.join("\n")));
                }
            }
        });
    }
}
