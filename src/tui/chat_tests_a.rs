use super::*;
use serde_json::json;

/// Test Chat: independent channels + a full Session.
pub(super) fn test_chat() -> Chat {
    test_chat_home(std::env::temp_dir())
}

/// Joined text of every slash output bucket (confirm + error + info).
pub(super) fn all_slash_text(chat: &Chat) -> String {
    chat.slash_lines
        .iter()
        .chain(&chat.slash_error_lines)
        .chain(&chat.slash_info_lines)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Let the `settle` blink expire (D87). A finished turn's last message stays
/// live for one 120 ms window so its completion row can wear the accent and then
/// rest — freezing it mid-blink would print the accent into scrollback for good.
/// Any test asserting the *final* scrollback state ticks past the window first,
/// exactly as the host does 120 ms later.
pub(super) fn past_settle(chat: &mut Chat) {
    while chat.settling() {
        chat.tick();
    }
}

/// Segments covered by the latest settled checkpoint (checkpoint-equivalent read of the old aggregate field).
pub(super) fn settled_segments(chat: &Chat) -> usize {
    chat.doc.settled_marks.last().map_or(0, |m| m.segments)
}

/// A Chat with its own home (the unique dir for slash tests, so transcript/task storage is not
/// shared with other tests). cwd points at the same home: the persistence paths of /model /think /theme etc.
/// write into `{cwd}/.bingo` and must never pollute the repo's real config.
pub(super) fn test_chat_home(home: std::path::PathBuf) -> Chat {
    let _ = std::fs::create_dir_all(&home);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let core = crate::app::AppCore::start(Default::default());
    let session = Arc::new(Session {
        client: crate::api::client::Client::new(
            "test-key".to_string(),
            "https://example.com".to_string(),
        ),
        runtime: crate::query::Runtime::new("test-model".to_string(), None, Default::default()),
        permission_mode: PermissionMode::Default,
        settings: crate::settings::Settings::default(),
        system: Vec::new(),
        depth: 0,
        cwd: Arc::new(std::sync::Mutex::new(home.clone())),
        home: home.clone(),
        // Hermetic: scoped writes must never touch the real user config.
        user_config_dir: home.join(".config"),
        quiet: true,
        compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: core.watch(),
        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&home, "test")),
        expand_tasks: tokio::sync::watch::channel(false).0,
        agents: core.agents(),
        channels: core.channels(),
        turns: core.turns(),
        queue: core.queue(),
        submit: core.submit(),
        interactions: core.interactions(),
        mail: core.mail(),
        operations: core.operations(),
        instance: None,
        attachments: crate::api::image::Attachments::new(),
    });
    session.agents.set_events(crate::ui::EventSink::new(
        crate::ui::ConvKey::Main,
        events_tx.clone(),
    ));
    Chat::new(
        session,
        crate::ui::EventSink::new(crate::ui::ConvKey::Main, events_tx),
        events_rx,
        Theme::dark(),
        crate::tui::theme::ThemeSetting::Auto,
        None,
    )
}

#[test]
fn share_rebind_failure_detaches_the_previous_store() {
    let home = std::env::temp_dir().join(format!("bingo-share-rebind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut chat = test_chat_home(home.clone());
    let initial = crate::transcript::create(&home, &home).unwrap();
    initial
        .append(&crate::api::types::Message::user_text("active"))
        .unwrap();
    let store = crate::share::ShareStore::load_or_create(
        &crate::share::shares_dir(&home).join(format!("{}.json", initial.name())),
    )
    .unwrap();
    chat.session.agents.attach_share(store.clone());
    chat.session.channels.attach_share(store);
    let destination = initial.rename("destination").unwrap();
    let destination_share =
        crate::share::shares_dir(&home).join(format!("{}.json", destination.name()));
    std::fs::create_dir_all(&destination_share).unwrap();

    chat.attach_share_to_transcript(Some(&destination));

    chat.session.channels.settle_now();
    assert!(!chat.session.agents.has_share());
    assert!(!chat.session.channels.has_share());
    assert!(
        chat.warnings
            .iter()
            .any(|(_, warning)| warning.contains("share store unavailable"))
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn slash_gc_cleans_storage_and_reports_the_policy() {
    let home = std::env::temp_dir().join(format!("bingo-slash-gc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let stale = crate::storage::transcripts_dir(&home).join("stale.jsonl");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "{}").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&stale)
        .unwrap();
    file.set_modified(
        std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 24 * 60 * 60),
    )
    .unwrap();
    let mut chat = test_chat_home(home.clone());

    assert!(chat.run_slash("gc"));

    assert!(!stale.exists());
    assert!(!chat.pinned_panels.iter().any(|(id, _)| id == "gc"));
    assert!(
        all_slash_text(&chat).contains("cleaned 1 transcript(s)")
            && all_slash_text(&chat).contains("TTL 30 days")
            && all_slash_text(&chat).contains("latest 100 inactive sessions kept")
            && all_slash_text(&chat).contains("24-hour activity grace")
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn slash_cd_updates_session_and_tool_context_cwd() {
    let root = std::env::temp_dir().join(format!("bingo-slash-cd-updates-{}", std::process::id()));
    let start = root.join("start");
    let target = root.join("target");
    std::fs::create_dir_all(&start).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let target = std::fs::canonicalize(target).unwrap();
    let mut chat = test_chat_home(start.clone());

    assert_eq!(chat.session.cwd(), start);
    assert!(chat.run_slash(&format!("cd {}", target.display())));
    assert_eq!(chat.session.cwd(), target);
    assert_eq!(chat.cwd, target.display().to_string());
    let ctx = crate::query::tool_context(&chat.session, &crate::query::headless_hooks()).unwrap();
    assert_eq!(ctx.cwd, target);
    assert!(all_slash_text(&chat).contains("✓ working directory:"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn slash_cd_rejects_missing_directory_without_changing_cwd() {
    let root = std::env::temp_dir().join(format!("bingo-slash-cd-missing-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut chat = test_chat_home(root.clone());

    assert!(chat.run_slash("cd missing"));
    assert_eq!(chat.session.cwd(), root);
    assert!(all_slash_text(&chat).contains("code=BAD_ARGUMENT"));

    std::fs::remove_dir_all(root).unwrap();
}

/// Banner truncation chain (update-banner spec §1.3): full / drop the available clause / command only / hidden.
#[test]
fn banner_segments_width_tiers() {
    let full = banner_segments("0.3.0", 50).unwrap();
    assert_eq!(full.0, "   New version ");
    assert_eq!(full.1, "v0.3.0");
    assert_eq!(full.2, " available — run ");
    assert_eq!(full.3, "bingo update");
    // The full line fits (width <50 but full_len ≤ width) → still full
    assert_eq!(banner_segments("0.3.0", 51).unwrap().2, " available — run ");
    // 43-49: the longest version v0.12.34 does not fit whole → drop the available clause
    let short = banner_segments("0.12.34", 49).unwrap();
    assert_eq!(short.2, " — run ");
    assert_eq!(short.3, "bingo update");
    // ≥15: command only
    let cmd_only = banner_segments("0.3.0", 15).unwrap();
    assert!(cmd_only.0.is_empty() && cmd_only.1.is_empty());
    assert_eq!(cmd_only.3, "bingo update");
    // <15: hidden
    assert!(banner_segments("0.3.0", 14).is_none());
}

#[test]
fn banner_line_width_tiers() {
    assert_eq!(
        banner_line("0.3.0", 50).unwrap(),
        "   New version v0.3.0 available — run bingo update"
    );
    assert_eq!(
        banner_line("0.12.34", 49).unwrap(),
        "   New version v0.12.34 — run bingo update"
    );
    assert_eq!(banner_line("0.3.0", 15).unwrap(), "   bingo update");
    assert!(banner_line("0.3.0", 14).is_none());
}

/// Breathing-color pure function (update-banner spec §2.3/anchor 2): phase 0 = rest (trough), 45 = peak,
/// 90 = back to rest; 0→45 strictly rises, 45→90 strictly falls; motion off → always rest; light theme takes the deep-orange stops.
#[test]
fn update_color_breathing_wave() {
    let dark = Theme::dark();
    let rest = Color::Rgb(215, 119, 87);
    let peak = Color::Rgb(232, 137, 107);
    assert_eq!(
        update_color(&dark, 0, false),
        rest,
        "starts at the trough (no jump)"
    );
    assert_eq!(
        update_color(&dark, 45, false),
        peak,
        "phase 45 = peak (exactly t=1)"
    );
    assert_eq!(update_color(&dark, 90, false), rest);
    assert_eq!(update_color(&dark, 135, false), peak, "cycle wraps around");
    assert_eq!(update_color(&dark, 180, false), rest);
    // Monotonicity (red channel)
    let r = |f: u64| -> u8 {
        match update_color(&dark, f, false) {
            Color::Rgb(r, _, _) => r,
            _ => panic!("a truecolor theme should return Rgb"),
        }
    };
    assert!(
        r(15) > r(0) && r(30) > r(15) && r(45) > r(30),
        "0→45 strictly rises"
    );
    assert!(
        r(60) < r(45) && r(75) < r(60) && r(90) < r(75),
        "45→90 strictly falls"
    );
    // motion off → always rest (the indicator stays, it just stops)
    assert_eq!(update_color(&dark, 45, true), rest);
    assert_eq!(update_color(&dark, 999, true), rest);
    // Light theme → deep-orange stops (rest #B05227 / peak #9A4A24); the bright orange is disabled
    let light = Theme::light();
    assert_eq!(update_color(&light, 0, false), Color::Rgb(176, 82, 39));
    assert_eq!(update_color(&light, 45, false), Color::Rgb(154, 74, 36));
    for f in [0u64, 22, 45, 67, 90] {
        assert_ne!(
            update_color(&light, f, false),
            Color::Rgb(215, 119, 87),
            "the light theme must not use the bright-orange stops (spec §2.2)"
        );
    }
}

/// 256-color discrete two-step (update-banner spec §2.4): 60-frame cycle, peak 400ms (12 frames) → rest.
/// (`downgrade_to_256` is a private theme.rs method; the test hand-builds an Indexed theme to simulate the downgrade.)
#[test]
fn update_color_256_discrete_two_step() {
    let mut d256 = Theme::dark();
    d256.claude = Color::Indexed(167); // dark rest approximation
    d256.claude_strong = Color::Indexed(173); // dark peak approximation
    let f0 = update_color(&d256, 0, false);
    let f11 = update_color(&d256, 11, false);
    let f12 = update_color(&d256, 12, false);
    let f59 = update_color(&d256, 59, false);
    let f60 = update_color(&d256, 60, false);
    assert_eq!(f0, f11, "peak phase is contiguous (frames 0-11)");
    assert_eq!(f12, f59, "rest phase is contiguous (frames 12-59)");
    assert_ne!(f0, f12, "the two stops differ");
    assert_eq!(f60, f0, "the 60-frame cycle wraps");
    assert!(
        matches!(update_color(&d256, 5, false), Color::Indexed(_)),
        "the 256-color downgrade outputs Indexed"
    );
}

/// Welcome-card rendering (update-banner anchors 1/6/9): with a banner row (including the two-segment form); the no-banner layout regression is unchanged;
/// narrow screens keep only `bingo update`.
#[test]
fn welcome_card_banner_rendering() {
    let theme = Theme::dark();
    let color = Color::Rgb(215, 119, 87);
    let with = welcome_card_rows(&theme, "m", "d", "/cwd", 80, Some(("0.3.0", color)), false);
    let texts: Vec<String> = with.iter().map(|r| r.line.plain_text()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("New version v0.3.0 available — run bingo update")),
        "the full-tier banner must fit inside the card: {texts:?}"
    );
    // The banner sits directly above the version-identity row (adjacent, no blank line between)
    let banner_idx = texts
        .iter()
        .position(|t| t.contains("New version"))
        .unwrap();
    assert!(
        texts[banner_idx + 1].contains("bingo v"),
        "the banner must sit right below the identity row"
    );
    // No banner → the layout matches the current one (regression)
    let without = welcome_card_rows(&theme, "m", "d", "/cwd", 80, None, false);
    assert_eq!(
        with.len(),
        without.len() + 2,
        "banner + the blank row above = 2 rows"
    );
    // Narrow (inner 15): command only; "New version" must not appear
    let narrow = welcome_card_rows(&theme, "m", "d", "/c", 17, Some(("0.3.0", color)), false);
    let narrow_texts: Vec<String> = narrow.iter().map(|r| r.line.plain_text()).collect();
    assert!(narrow_texts.iter().any(|t| t.contains("bingo update")));
    assert!(!narrow_texts.iter().any(|t| t.contains("New version")));
    // Very narrow (inner <15): the banner is hidden, layout matches the no-banner one
    let tiny = welcome_card_rows(&theme, "m", "d", "/c", 16, Some(("0.3.0", color)), false);
    assert_eq!(
        tiny.len(),
        without.len(),
        "the banner is hidden when inner<15"
    );
}

/// Breathing window (update-banner anchors 3/5): active throughout the 270 frames (has_dynamic_rows),
/// idle outside the window; a keypress in the window → stops immediately; motion off → never active.
#[test]
fn update_banner_animation_window_and_key_stop() {
    let mut chat = test_chat();
    chat.update_banner = Some("0.3.0".into());
    chat.update_banner_start = 0;
    chat.tick = 0;
    assert!(chat.update_anim_active());
    assert!(
        chat.has_dynamic_rows(),
        "the frame loop keeps dirty set inside the breathing window"
    );
    chat.tick = 269;
    assert!(
        chat.update_anim_active(),
        "still active on the window's last frame"
    );
    chat.tick = 270;
    assert!(!chat.update_anim_active(), "resting outside the window");
    assert!(
        !chat.has_dynamic_rows(),
        "back to idle outside the window (zero writes)"
    );
    // A keypress in the window → stops immediately (P1)
    chat.update_banner_start = 0;
    chat.tick = 50;
    assert!(chat.update_anim_active());
    let _ = chat.on_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(
        !chat.update_anim_active(),
        "the first keypress in the window stops it immediately"
    );
    // motion off → never active (the banner stays as a static rest)
    let mut chat2 = test_chat();
    chat2.update_banner = Some("0.3.0".into());
    chat2.motion = crate::tui::motion::Motion::new(false);
    chat2.tick = 10;
    assert!(!chat2.update_anim_active());
    assert!(!chat2.has_dynamic_rows());
}

pub(super) fn tool_activity() -> Activity {
    let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running("Bash", "")));
    hint.set_content(vec![
        Line::plain("output line 1"),
        Line::plain("output line 2"),
    ]);
    hint.expand_hint = Some("ctrl+o to expand".to_string());
    hint
}

pub(super) fn msg(role: Role, text: &str) -> UiMessage {
    UiMessage {
        speaker: None,
        role,
        text: text.to_string(),
        at: 0,
        activities: Vec::new(),
        insert_points: Vec::new(),
        groups: Vec::new(),
        group_of: Vec::new(),
    }
}

/// Simulates the component layer: build_rows + scroll + viewport slice → visible text.
pub(super) fn visible(chat: &mut Chat, width: usize, height: usize) -> String {
    chat.build_rows(width);
    chat.reconcile_scroll(height.saturating_sub(3));
    let scroll = chat.scroll;
    let rows: Vec<String> = chat
        .doc
        .rows
        .iter()
        .skip(scroll)
        .take(height.saturating_sub(3))
        .map(|r| r.line.plain_text())
        .filter(|l| !l.trim().is_empty())
        .collect();
    rows.join("\n")
}

pub(super) fn start_group(chat: &mut Chat) {
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    for path in ["a.md", "b.md"] {
        chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            input: json!({"file_path": path}),
            standalone: false,
        });
        chat.drain_events();
    }
}

pub(super) fn finish_turn(chat: &mut Chat) {
    chat.conv.stream_msg = Some(0);
    chat.events.send(UiEvent::TurnEnd);
    chat.drain_events();
    chat.conv.stream_msg = None;
}

/// start_group + tool completion (with explicit summaries, like the old build_group_chat(true)).
pub(super) fn start_group_done(chat: &mut Chat) {
    start_group(chat);
    for (summary, out) in [("Read a.md", "l1\nl2\nl3"), ("Read b.md", "x\ny")] {
        chat.events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: "Read".into(),
                summary: summary.into(),
                output: out.into(),
                status: crate::query::ToolCallStatus::Done,
                duration_ms: 0,
                diff: None,
            }));
    }
    chat.drain_events();
}

/// No transcript row is wider than the width it was built for. The reply
/// marker is prepended *after* the markdown is wrapped, so rendering the text
/// at the full width made every filled first line `width + 2`: the viewport
/// clipped the overhang (two characters gone with no sign they existed) and
/// scrollback would have wrapped it onto a second physical row, breaking the
/// one-document-row-per-terminal-row invariant the whole write-once design
/// rests on.
#[test]
fn no_row_overflows_the_build_width() {
    for width in [40usize, 80, 100] {
        let mut chat = test_chat();
        // Long enough that some line must fill the width exactly.
        let long = "lockfile pins rewritten alongside the version bump ".repeat(8);
        chat.conv.messages.push(msg(Role::User, &long));
        chat.conv.messages.push(msg(Role::Assistant, &long));
        chat.build_rows(width);
        for (i, row) in chat.doc.rows.iter().enumerate() {
            let w = text_width(&row.line.plain_text());
            assert!(
                w <= width,
                "row {i} is {w} wide at width {width}: {:?}",
                row.line.plain_text()
            );
        }
    }
}

/// A subagent's watch row is the one place in the transcript with many named
/// speakers, so it wears their faces — the portrait spans the header and the
/// result row, which is the height the block already had. The `⎿` connector is
/// what it costs, and only where a face is actually drawn: a chip terminal has
/// nothing to spend, so it keeps `◉` and the connector untouched.
#[test]
fn agent_watch_rows_wear_the_instance_face_only_where_images_place() {
    let watch = |chat: &mut Chat| {
        chat.conv.messages.push(msg(Role::Assistant, ""));
        // The row belongs to the turn that spawned the agent (D94): outside a
        // running turn an agent's lifecycle no longer writes into main at all,
        // and what this test is about is how the row looks, not whether it exists.
        chat.conv.stream_msg = Some(chat.conv.messages.len() - 1);
        chat.apply_event(UiEvent::WatchEvent {
            label: "林夏 · UI review".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 200 chars".into()),
            duration_ms: 0,
            payload: None,
            signal: None,
            notifies_main: false,
            dispatch: true,
        });
        chat.build_rows(80);
        chat.doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
    };

    let mut chip = test_chat();
    chip.chat_avatars = true;
    let rows = watch(&mut chip);
    assert!(
        rows.iter().any(|r| r.contains("◉ @林夏: UI review")),
        "chip terminals keep the glyph: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("⎿")),
        "and keep the connector: {rows:?}"
    );
    assert!(
        chip.faces.len() <= 2,
        "no portrait was claimed for a terminal that cannot draw one"
    );

    let mut placed = test_chat();
    placed.chat_avatars = true;
    placed.image_cap = Some(ImageCap::default_cells());
    let rows = watch(&mut placed);
    let header = rows
        .iter()
        .find(|r| r.contains("@林夏: UI review"))
        .unwrap_or_else(|| panic!("watch row present: {rows:?}"));
    assert!(
        header.contains(gfx::PLACEHOLDER) && !header.contains('◉'),
        "the face replaces the glyph: {header:?}"
    );
    assert!(
        placed.faces.contains(&crate::tui::avatar::index_of("林夏")),
        "the instance's face is recorded for transmission"
    );
}

/// Rewritten for D99: the band retires. It existed because the console had no
/// gutter — "the main chat has no gutter, so the face goes overhead" — and the
/// console has one now, so a band would have drawn the same speaker's portrait
/// twice on the same message. What names the speaker in @main is the gutter,
/// and it names them without the switch and without a name row.
#[test]
fn the_console_names_its_speakers_in_the_gutter_and_not_above_them() {
    let mut chat = test_chat();
    chat.chat_avatars = true;
    chat.conv.messages.push(msg(Role::User, "hi"));
    chat.conv.messages.push(msg(Role::Assistant, "hello"));
    chat.build_rows(80);
    let rows: Vec<String> = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text().trim().to_string())
        .collect();
    assert!(
        !rows
            .iter()
            .any(|r| r.ends_with("You") || r.ends_with(crate::channels::MAIN_NAME)),
        "no name row above a message, switch or no switch: {rows:?}"
    );
    assert!(rows.iter().any(|r| r.starts_with("U  ❯ hi")), "{rows:?}");
    assert!(rows.iter().any(|r| r.starts_with("M  ⏺ hello")), "{rows:?}");
    let expected: HashSet<usize> = [
        avatar::index_of(crate::channels::USER_NAME),
        avatar::MAIN_INDEX,
    ]
    .into_iter()
    .collect();
    assert_eq!(chat.faces, expected, "both faces recorded for transmission");
}

/// Rewritten twice, honestly both times. D99 narrowed the switch to the band
/// and the watch row's portrait and let the gutter run unconditionally; D110
/// is the user's ruling the other way — **every** avatar follows
/// `experimental.chatAvatars` — so "off" (the default) now means what it says:
/// no band, no watch-row portrait, and no gutter, faces or chips, anywhere.
#[test]
fn without_the_switch_the_transcript_wears_no_band() {
    let mut chat = test_chat();
    assert!(!chat.chat_avatars, "off unless a settings layer asks");
    chat.image_cap = Some(ImageCap::default_cells());
    chat.conv.messages.push(msg(Role::User, "hi"));
    chat.conv.messages.push(msg(Role::Assistant, ""));
    // Same as above: a watch row exists in main only as the running turn's own
    // tool row (D94).
    chat.conv.stream_msg = Some(chat.conv.messages.len() - 1);
    chat.apply_event(UiEvent::WatchEvent {
        label: "林夏 · UI review".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Running,
        detail: Some("produced 200 chars".into()),
        duration_ms: 0,
        payload: None,
        signal: None,
        notifies_main: false,
        dispatch: true,
    });
    chat.build_rows(80);
    let rows: Vec<String> = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text().trim().to_string())
        .collect();
    assert!(
        !rows
            .iter()
            .any(|r| r.ends_with("You") || r.ends_with(crate::channels::MAIN_NAME)),
        "no band names a speaker over a message: {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("◉ @林夏: UI review")),
        "the watch row keeps its glyph: {rows:?}"
    );
    // The gutter follows the same switch (D110, user ruling: every avatar
    // does): off means no portrait cells anywhere, no face claimed for
    // transmission, and the message column opening at the left edge — the
    // transcript reads as if the avatar machinery did not exist.
    assert!(
        !rows.iter().any(|r| r.contains(gfx::PLACEHOLDER)),
        "no placeholder cells with the switch off: {rows:?}"
    );
    assert!(chat.faces.is_empty(), "{:?}", chat.faces);
}

/// Task-family / AskUserQuestion calls are not shown in the transcript
/// (renderToolUseMessage = null; the task panel / dialog shows them).
#[test]
fn hidden_tools_produce_no_activities() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    for name in [
        "TaskCreate",
        "TaskUpdate",
        "TaskGet",
        "TaskList",
        "AskUserQuestion",
    ] {
        chat.events.send(UiEvent::ToolStart { name: name.into() });
        chat.drain_events();
        chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: name.into(),
            input: json!({}),
            standalone: false,
        });
        chat.drain_events();
    }
    assert!(
        chat.conv.messages[0].activities.is_empty(),
        "hidden tools leave no activities: {:?}",
        chat.conv.messages[0].activities
    );
    assert!(
        chat.conv.pending_tools.is_empty(),
        "the pending FIFO stays matched"
    );
    // Visible tools still render normally.
    chat.events.send(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: json!({"command": "ls"}),
        standalone: false,
    });
    chat.drain_events();
    assert_eq!(
        chat.conv.messages[0].activities.len(),
        1,
        "Bash renders normally"
    );
}

#[tokio::test]
async fn chat_tasks_reflect_store_changes() {
    // The TUI task area's data source = live snapshot of the disk store (the data layer of the tick broadcast chain).
    let mut chat = test_chat();
    assert!(chat.tasks().is_empty());
    let store = chat.session.tasks.clone();
    let id = store
        .create(&crate::tasks::Task {
            id: String::new(),
            subject: "fix flicker".into(),
            description: String::new(),
            active_form: None,
            status: crate::tasks::TaskStatus::Pending,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: Default::default(),
        })
        .await
        .unwrap();
    chat.refresh_tasks();
    assert_eq!(chat.tasks_cache.len(), 1);
    assert_eq!(chat.tasks_cache[0].text, "fix flicker");
    store
        .update(
            &id,
            &crate::tasks::TaskPatch {
                status: Some(crate::tasks::TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    chat.refresh_tasks();
    assert_eq!(chat.tasks_cache[0].status, TodoStatus::InProgress);
    store.delete(&id).await.unwrap();
    chat.refresh_tasks();
    assert!(chat.tasks_cache.is_empty());
}

/// Creates a task and returns its id (writes to the temp store).
async fn create_task(chat: &Chat, subject: &str) -> String {
    chat.session
        .tasks
        .create(&crate::tasks::Task {
            id: String::new(),
            subject: subject.into(),
            description: String::new(),
            active_form: None,
            status: crate::tasks::TaskStatus::Pending,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: Default::default(),
        })
        .await
        .unwrap()
}

/// Auto-opened task area (TaskCreate signal semantics): all done → hide + transient line;
/// new task → reappears; all done again → hides again; once hidden, idle writes nothing.
#[tokio::test]
async fn auto_todo_hides_when_all_done() {
    let mut chat = chat_with_history("todo-auto");
    let store = chat.session.tasks.clone();
    let id = create_task(&chat, "t1").await;
    chat.tasks_visible = true;
    chat.tasks_auto = true;
    chat.refresh_tasks();
    assert!(
        chat.tasks_visible,
        "the auto panel shows when there are active items"
    );
    assert!(!chat.task_lines().is_empty());

    store
        .update(
            &id,
            &crate::tasks::TaskPatch {
                status: Some(crate::tasks::TaskStatus::Completed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    chat.refresh_tasks();
    assert!(
        !chat.tasks_visible,
        "the auto panel hides when everything completes"
    );
    assert!(!chat.tasks_auto);
    assert!(chat.task_lines().is_empty());
    assert!(
        chat.slash_lines
            .iter()
            .any(|l| l.contains("✓ 1/1 tasks done")),
        "a transient row is pushed at the hiding moment: {:?}",
        chat.slash_lines
    );
    assert!(
        !chat.has_dynamic_rows(),
        "after hiding, the task area does not drive the tick"
    );

    // Create another task (the expand signal reopens the panel) → reappears; all done again → hides again.
    let id2 = create_task(&chat, "t2").await;
    chat.tasks_visible = true;
    chat.tasks_auto = true;
    chat.refresh_tasks();
    assert!(
        chat.tasks_visible,
        "the auto panel reappears with a new task"
    );
    store
        .update(
            &id2,
            &crate::tasks::TaskPatch {
                status: Some(crate::tasks::TaskStatus::Completed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    chat.refresh_tasks();
    assert!(
        !chat.tasks_visible,
        "hides again once everything completes again"
    );
}

/// Panel opened manually with ctrl+t: kept even when everything is done (the user explicitly wants to see it), no transient line.
#[tokio::test]
async fn manual_todo_stays_when_all_done() {
    let mut chat = chat_with_history("todo-manual");
    let id = create_task(&chat, "t1").await;
    chat.session
        .tasks
        .update(
            &id,
            &crate::tasks::TaskPatch {
                status: Some(crate::tasks::TaskStatus::Completed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    ctrl(&mut chat, 't');
    assert!(chat.tasks_visible, "manual open shows it");
    assert!(!chat.tasks_auto, "manual open is not auto");
    chat.refresh_tasks();
    let lines = chat.task_lines();
    let joined: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
    assert!(joined[0].contains("todo · 1/1 tasks"), "{joined:?}");
    assert!(joined.iter().any(|l| l.starts_with("☒ ")), "{joined:?}");
    assert!(
        chat.slash_lines.is_empty(),
        "a manual panel is its own feedback; no transient row is pushed: {:?}",
        chat.slash_lines
    );
}

/// `/tasks` explicit request: outputs the ☒ list even when everything is done, never falsely reports "no background tasks".
#[tokio::test]
async fn slash_tasks_shows_done_list() {
    let mut chat = chat_with_history("todo-slash");
    let id = create_task(&chat, "t1").await;
    chat.session
        .tasks
        .update(
            &id,
            &crate::tasks::TaskPatch {
                status: Some(crate::tasks::TaskStatus::Completed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    chat.slash_tasks();
    let joined = chat.slash_info_lines.join("\n");
    assert!(joined.contains("☒ t1"), "{joined:?}");
    assert!(!joined.contains("no background tasks"), "{joined:?}");
}

#[test]
fn click_toggles_tool_activity() {
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        activities: vec![tool_activity()],
        ..msg(Role::Assistant, "reply")
    });
    chat.build_rows(100);
    assert!(
        !chat.doc.click_ranges.is_empty(),
        "build_rows populates ranges"
    );

    let start = {
        let range = &chat.doc.click_ranges[0];
        assert!(matches!(
            &range.target,
            ClickTarget::Activity { path, .. } if path == &vec![0]
        ));
        range.start
    };
    assert!(chat.doc_click(start), "click on header expands");
    assert!(chat.conv.messages[0].activities[0].expanded);
    assert!(chat.doc_click(start), "click collapses again");
    assert!(!chat.conv.messages[0].activities[0].expanded);
}

#[test]
fn click_outside_ranges_is_noop() {
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        activities: vec![tool_activity()],
        ..msg(Role::Assistant, "reply")
    });
    chat.build_rows(100);
    assert!(!chat.doc_click(999), "no range -> no toggle");
}

/// Running status-row data (ActivityIndicator): None when idle;
/// when busy, prefer the running tool's summary, then a thinking word, fall back to Working.
#[test]
fn running_status_verb_priority() {
    let mut chat = test_chat();
    assert_eq!(chat.running_status(), None, "no status row when idle");

    chat.start_test_turn();
    chat.conv.turn_started = Some(std::time::Instant::now());
    let verb = chat.running_status().expect("busy status").verb;
    assert_eq!(verb, "Working", "fallback when nothing is active");

    let mut tool = tool_activity();
    if let ActivityKind::Tool(t) = &mut tool.kind {
        t.summary = "$ cargo test".to_string();
    }
    chat.conv.messages.push(UiMessage {
        speaker: None,
        activities: vec![tool],
        ..msg(Role::Assistant, "")
    });
    let verb = chat.running_status().expect("busy status").verb;
    assert_eq!(verb, "$ cargo test", "a running tool's summary wins");

    // A running Watch (subagent/background task) verb = its label (CC ActivityIndicator
    // shows the agent activeForm): after tools, before thinking.
    chat.conv.messages[0].activities.clear();
    chat.conv.messages[0]
        .activities
        .push(Activity::new(ActivityKind::Watch(WatchCall {
            label: "scout · listing desktop dir contents".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 43 chars".into()),
            duration_ms: 0,
            progress: Vec::new(),
            run_stats: None,
        })));
    let verb = chat.running_status().expect("busy status").verb;
    assert_eq!(
        verb, "scout · listing desktop dir contents",
        "a Running Watch's verb = its label"
    );

    // A Done Watch no longer claims the verb (falls through to thinking/Working).
    if let ActivityKind::Watch(w) = &mut chat.conv.messages[0].activities[0].kind {
        w.status = WatchState::Done;
    }
    let verb = chat.running_status().expect("busy status").verb;
    assert_ne!(
        verb, "Agent: listing desktop dir contents",
        "a Done Watch does not occupy the verb"
    );

    chat.conv.messages[0].activities.clear();
    chat.apply_turn_start();
    // TurnStart appends a new message (index 1): the placeholder thinking lives there.
    let stage = match &chat.conv.messages[1].activities[0].kind {
        ActivityKind::Thinking(t) => t.stage,
        _ => unreachable!(),
    };
    let verb = chat.running_status().expect("busy status").verb;
    assert_eq!(verb, stage, "thinking quip words");
}

/// bash-mode toggle: `!` on empty input enters, `!` never enters the input,
/// `!` inserts normally when the input is non-empty, backspace on empty input exits.
#[test]
fn bang_toggles_bash_mode() {
    let mut chat = test_chat();
    assert!(!chat.bash_mode);
    assert!(chat.on_key(KeyCode::Char('!'), KeyModifiers::empty()));
    assert!(chat.bash_mode, "! enters bash mode");
    assert!(chat.input.is_empty(), "! itself does not insert input");
    assert!(chat.on_key(KeyCode::Char('l'), KeyModifiers::empty()));
    assert_eq!(chat.input, "l");
    assert!(chat.on_key(KeyCode::Char('!'), KeyModifiers::empty()));
    assert_eq!(chat.input, "l!", "with non-empty input, ! inserts normally");
    assert!(chat.bash_mode, "non-empty input does not exit bash mode");
    assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
    assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
    assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
    assert!(
        !chat.bash_mode,
        "backspace on an empty input exits bash mode"
    );
}

/// `!` commands (standalone tool activity): not part of collapse groups, expanded by default when done,
/// preview = the output itself (stripped of the `$ cmd` echo and the `[Exited with code N]` footnote).
#[test]
fn bash_preview_expands_with_output() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    chat.events.send(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: json!({"command": "ls"}),
        standalone: true,
    });
    chat.drain_events();
    assert!(
        chat.conv.messages[0].groups.is_empty(),
        "standalone messages do not group"
    );
    chat.events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Bash".into(),
            summary: "$ ls".into(),
            output: "$ ls\nREADME.md\nsrc\n[Exited with code 0]".into(),
            status: crate::query::ToolCallStatus::Done,
            duration_ms: 5,
            diff: None,
        }));
    chat.drain_events();
    let a = &chat.conv.messages[0].activities[0];
    assert!(a.expanded, "the output preview is expanded by default");
    let text: Vec<String> = a.content.iter().map(|l| l.plain_text()).collect();
    assert_eq!(
        text,
        vec!["README.md", "src"],
        "the preview drops the echo and the exit code: {text:?}"
    );
}

/// Model-driven Bash (standalone=false) still folds into a group as before.
#[test]
fn model_bash_still_folds_into_group() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    chat.events.send(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: json!({"command": "cargo test"}),
        standalone: false,
    });
    chat.drain_events();
    assert_eq!(
        chat.conv.messages[0].groups.len(),
        1,
        "model-driven messages still group"
    );
}

/// bash-mode submit: the user message carries the `!` prefix, the command runs as a tool activity and finishes normally
/// (respondToBashCommands=false → no model call; the turn ends and busy resets).
#[tokio::test]
async fn bash_submit_runs_command_and_ends_turn() {
    let core = crate::app::AppCore::start(Default::default());
    let session = Arc::new(Session {
        client: crate::api::client::Client::new("k".into(), "http://127.0.0.1:9".into()),
        runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
        permission_mode: PermissionMode::BypassPermissions,
        settings: crate::settings::Settings {
            respond_to_bash_commands: Some(false),
            ..Default::default()
        },
        system: Vec::new(),
        depth: 0,
        cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
        home: std::env::temp_dir(),
        user_config_dir: std::env::temp_dir().join(".config"),
        quiet: true,
        compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: core.watch(),
        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
        expand_tasks: tokio::sync::watch::channel(false).0,
        agents: core.agents(),
        channels: core.channels(),
        turns: core.turns(),
        queue: core.queue(),
        submit: core.submit(),
        interactions: core.interactions(),
        mail: core.mail(),
        operations: core.operations(),
        instance: None,
        attachments: crate::api::image::Attachments::new(),
    });
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let mut chat = Chat::new(
        session,
        crate::ui::EventSink::new(crate::ui::ConvKey::Main, events_tx),
        events_rx,
        Theme::dark(),
        crate::tui::theme::ThemeSetting::Auto,
        None,
    );
    chat.bash_mode = true;
    chat.input = "echo hello".to_string();
    chat.submit();
    assert!(chat.bash_mode, "bash mode stays on after submit");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        chat.drain_all();
        if !chat.conv.busy && !chat.conv.messages.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the turn did not end within the timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        chat.conv.messages[0].text, "!echo hello",
        "the user message carries the ! prefix"
    );
    let done_tool = chat.conv.messages[1].activities.iter().any(|a| {
        matches!(&a.kind, ActivityKind::Tool(t)
                if t.name == "Bash" && t.status == ToolStatus::Done)
    });
    assert!(done_tool, "the Bash tool activity closes as Done");
    let preview = &chat.conv.messages[1].activities[0];
    assert!(
        preview.expanded,
        "the ! command's output preview is expanded"
    );
    assert!(
        preview.content.iter().any(|l| l.plain_text() == "hello"),
        "the preview contains the command output: {:?}",
        preview
            .content
            .iter()
            .map(|l| l.plain_text())
            .collect::<Vec<_>>()
    );
    assert!(!chat.conv.busy, "turn ended");
}

fn thinking_text(hint: &Activity) -> String {
    hint.content
        .iter()
        .map(|l| l.plain_text().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// Thinking between tool rounds merges into one block when text has not interrupted (segments split by blank lines),
/// with later deltas continuing into the merged block.
#[test]
fn tool_turn_thinking_blocks_merge_until_text() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    chat.apply_event(UiEvent::ThinkingDelta("plan the fetch".into()));
    chat.apply_event(UiEvent::ToolStart {
        name: "WebFetch".into(),
    });
    chat.apply_event(UiEvent::ThinkingDelta("got it".into()));
    chat.apply_event(UiEvent::ThinkingDelta(", summarizing".into()));

    let acts = &chat.conv.messages[0].activities;
    assert_eq!(acts.len(), 2, "thinking merged + tool");
    let (first, tool) = (&acts[0], &acts[1]);
    assert!(matches!(&first.kind, ActivityKind::Thinking(t)
            if t.state == ThinkingState::Running && t.segments == 2));
    assert!(matches!(tool.kind, ActivityKind::Tool(_)));
    let text = thinking_text(first);
    assert!(text.contains("plan the fetch"), "first segment: {text}");
    assert!(
        text.contains("got it, summarizing"),
        "merged segment: {text}"
    );
}

/// Thinking after text interrupts opens a new block, no longer merging.
#[test]
fn thinking_after_text_opens_new_block() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
    chat.apply_event(UiEvent::TextDelta("body…".into()));
    chat.apply_event(UiEvent::ThinkingDelta("reflect".into()));

    let acts = &chat.conv.messages[0].activities;
    assert_eq!(acts.len(), 2, "two thinking blocks");
    let (first, second) = (&acts[0], &acts[1]);
    assert!(matches!(&first.kind, ActivityKind::Thinking(t) if t.segments == 1));
    assert!(matches!(&second.kind, ActivityKind::Thinking(t) if t.segments == 1));
    assert_eq!(thinking_text(first), "plan");
    assert_eq!(thinking_text(second), "reflect");
}

/// The thinking completion row (CC SystemTextMessage `✻ Churned for 40s`) renders at the end of the message:
/// after text and all activities; empty placeholder thinking (no content) produces no completion row.
#[test]
fn thinking_completion_line_renders_at_message_end() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
    let mut done = chat.conv.messages[0].activities[0].clone();
    if let ActivityKind::Thinking(t) = &mut done.kind {
        t.state = ThinkingState::Done;
        t.duration_ms = 3300;
        t.done_verb = Some("Baked");
    }
    chat.conv.messages[0].activities[0] = done;
    chat.conv.messages[0].text = "hello!".to_string();
    chat.apply_event(UiEvent::TurnEnd);
    chat.build_rows(100);
    let joined: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
    let lines: Vec<&str> = joined.iter().map(String::as_str).collect();
    let thinking = lines
        .iter()
        .position(|l| l.contains("✻ Thinking"))
        .expect("thinking block header");
    let reply = lines
        .iter()
        .position(|l| l.contains("hello"))
        .expect("reply text");
    let done_line = lines
        .iter()
        .position(|l| l.contains("✻ Baked for 3.3s"))
        .expect("completion line");
    assert!(
        thinking < reply && reply < done_line,
        "the completion line sits at the message end: {lines:?}"
    );

    // Empty placeholder thinking (no content) → no completion row.
    let mut chat2 = test_chat();
    chat2.apply_turn_start();
    let mut ph = chat2.conv.messages[0].activities[0].clone();
    if let ActivityKind::Thinking(t) = &mut ph.kind {
        t.state = ThinkingState::Done;
        t.duration_ms = 400;
    }
    chat2.conv.messages[0].activities[0] = ph;
    chat2.apply_event(UiEvent::TurnEnd);
    chat2.build_rows(100);
    let joined2: String = chat2
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !joined2.contains("for 0.4s"),
        "an empty placeholder has no completion line: {joined2}"
    );
}

/// The completion row only appears after the turn ends: with thinking Done but tools still running,
/// `✻ Baked for 0.4s` is not rendered, avoiding a contradiction with the bottom running-status row.
#[test]
fn thinking_completion_line_waits_for_turn_end() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
    chat.apply_event(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.build_rows(100);
    let rows: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
    assert!(
        !rows
            .iter()
            .any(|l| l.trim_start().starts_with("✻ ") && l.contains(" for ")),
        "no completion line mid-turn: {rows:?}"
    );
    chat.apply_event(UiEvent::TurnEnd);
    chat.build_rows(100);
    let rows: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
    assert!(
        rows.iter()
            .any(|l| l.trim_start().starts_with("✻ ") && l.contains(" for ")),
        "a completion line must appear after the turn: {rows:?}"
    );
}

/// Consecutive deltas within one turn continue the same block.
#[test]
fn single_turn_thinking_accumulates() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    chat.apply_event(UiEvent::ThinkingDelta("a".into()));
    chat.apply_event(UiEvent::ThinkingDelta("b".into()));

    let acts = &chat.conv.messages[0].activities;
    assert_eq!(acts.len(), 1);
    assert_eq!(thinking_text(&acts[0]), "ab");
}

/// Every message carries its send time — the same stamp brick every
/// conversation uses — while a still-streaming reply shows none, and a message
/// without a clock renders none. D93 moved it beside the message from under it;
/// the ordering asserted here holds either way, and
/// [`a_stamp_sits_beside_its_message_not_under_it`] pins the placement.
#[test]
fn messages_trail_their_send_time() {
    let at = 1_760_000_000u64;
    let want = crate::tui::buffer::stamp(at);
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at,
        ..msg(Role::User, "hello there")
    });
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at,
        ..msg(Role::Assistant, "the reply")
    });
    let joined = visible(&mut chat, 100, 40);
    let hello = joined.find("hello there").expect("user body");
    let reply = joined.find("the reply").expect("assistant body");
    let first = joined.find(&want).expect("user stamp");
    let last = joined.rfind(&want).expect("assistant stamp");
    assert!(hello < first && first < reply && reply < last, "{joined}");

    // While the reply is still streaming, its clock stays off the screen.
    chat.conv.stream_msg = Some(1);
    let joined = visible(&mut chat, 100, 40);
    assert_eq!(joined.matches(&want).count(), 1, "{joined}");
    chat.conv.stream_msg = None;

    // No clock (a test fixture, a legacy record) → no stamp row.
    chat.conv.messages.clear();
    chat.conv.messages.push(msg(Role::User, "undated"));
    let joined = visible(&mut chat, 100, 40);
    assert!(!joined.contains(&want), "{joined}");

    // Turn end restamps the streaming reply: the shown time is when the
    // reply landed, exactly as the workspace DM stamps it.
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at: 5,
        ..msg(Role::Assistant, "late reply")
    });
    chat.conv.stream_msg = Some(1);
    chat.apply_event(UiEvent::TurnEnd);
    assert!(chat.conv.messages[1].at > 5, "restamped at turn end");
}

/// D93: the stamp sits on the message's first row, flush right, and never on a
/// row of its own — a clock under every message cost a terminal line each and
/// read as a column of noise beside the words it was timing.
#[test]
fn a_stamp_sits_beside_its_message_not_under_it() {
    let at = 1_760_000_000u64;
    let want = crate::tui::buffer::stamp(at);
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at,
        ..msg(Role::User, "hello there")
    });
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at,
        ..msg(Role::Assistant, "the reply")
    });
    chat.build_rows(60);
    let rows: Vec<String> = chat
        .doc
        .rows
        .iter()
        .map(|row| row.line.plain_text())
        .collect();

    for body in ["hello there", "the reply"] {
        let row = rows
            .iter()
            .find(|row| row.contains(body))
            .unwrap_or_else(|| panic!("{body} row missing from {rows:?}"));
        assert!(
            row.trim_end().ends_with(&format!("  {want}")),
            "the stamp is flush right on the body's own row, two columns clear: {row:?}"
        );
    }
    assert!(
        rows.iter().all(|row| row.trim() != want),
        "no standalone stamp row survives: {rows:?}"
    );
}

/// Content wins. Where the row cannot hold body and stamp two columns apart,
/// the stamp is the thing that goes — nothing is wrapped or truncated to fit a
/// clock in.
#[test]
fn a_stamp_too_wide_for_its_row_is_dropped_not_squeezed() {
    let at = 1_760_000_000u64;
    let want = crate::tui::buffer::stamp(at);
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at,
        ..msg(Role::User, "a message with no room to spare")
    });
    chat.build_rows(18);
    let rows: Vec<String> = chat
        .doc
        .rows
        .iter()
        .map(|row| row.line.plain_text())
        .collect();
    assert!(
        rows.iter().all(|row| !row.contains(&want)),
        "the stamp gave way: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("a message")),
        "and the message did not: {rows:?}"
    );
    for row in &rows {
        assert!(
            crate::tui::line::text_width(row) <= 18,
            "no row overflows: {row:?}"
        );
    }
}

/// The alignment is in display columns, not characters: a CJK body lands the
/// stamp on the same column an ASCII one does.
#[test]
fn a_cjk_body_aligns_its_stamp_by_width() {
    let at = 1_760_000_000u64;
    let want = crate::tui::buffer::stamp(at);
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        at,
        ..msg(Role::User, "宽字符测试")
    });
    chat.build_rows(40);
    let row = chat
        .doc
        .rows
        .iter()
        .map(|row| row.line.plain_text())
        .find(|row| row.contains("宽字符测试"))
        .expect("the CJK body");
    assert!(row.trim_end().ends_with(&want), "{row:?}");
    // The bubble reserves its rightmost column, so the stamp lands on 39 — a
    // char-counting alignment would have run four columns past the edge.
    assert_eq!(crate::tui::line::text_width(&row), 39, "{row:?}");
}

/// Interleaved rendering: text and activities cross by insert point (model output in text → tool → text order).
#[test]
fn interleaves_text_and_activities_in_order() {
    let mut chat = test_chat();
    chat.conv.messages.push(UiMessage {
        speaker: None,
        text: "hello world".to_string(),
        activities: vec![tool_activity()],
        insert_points: vec![5],
        ..msg(Role::Assistant, "")
    });
    let joined = visible(&mut chat, 100, 40);
    let hello = joined.find("hello").expect("first text before tool");
    let tool = joined.find("Bash").expect("tool row");
    let world = joined.find("world").expect("trailing text after tool");
    assert!(hello < tool, "text before tool: {joined}");
    assert!(tool < world, "tool before trailing text: {joined}");
}

// ------------------------------------------------------------------
// Slash commands (/help /model /clear /exit /theme /rename /resume
// /permissions /skills /tasks /compact)
// ------------------------------------------------------------------

/// Input-layer interception: a leading / never starts a turn; /help lists commands, unknown ones get a hint.
#[test]
fn slash_intercepts_and_help_lists_commands() {
    let mut chat = test_chat();
    chat.input = "/help".to_string();
    chat.submit();
    assert!(!chat.conv.busy, "slash does not start a turn");
    let joined = chat.slash_info_lines.join("\n");
    for cmd in [
        "/clear", "/model", "/resume", "/rename", "/compact", "/exit",
    ] {
        assert!(joined.contains(cmd), "missing {cmd}: {joined}");
    }

    chat.input = "/nope".to_string();
    chat.submit();
    assert!(
        chat.slash_error_lines
            .iter()
            .any(|l| l.contains("unknown command")),
        "unknown commands land in the error rows: {:?}",
        chat.slash_error_lines
    );
}

/// picker-model.md commit E: /model's level one goes through the PickerModel core — ● marks the current provider,
/// number jump, and level two keeps its logic (digits never reach the input).
#[tokio::test]
async fn model_menu_level_one_uses_picker_core() {
    let mut chat = test_chat();
    // Level one: default + one named provider.
    let settings = crate::settings::Settings {
        api_key: Some("sk-main".into()),
        ..Default::default()
    };
    let mut s2 = settings.clone();
    s2.providers.insert(
        "deepseek".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: Some("sk-ds".into()),
            api_base_url: "https://api.deepseek.com".into(),
            supports_images: None,
            protocol: None,
            oauth: None,
        },
    );
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings(&s2).unwrap();

    chat.input = "/model".to_string();
    chat.submit();
    let menu = chat.model_menu.as_ref().expect("menu is open");
    assert_eq!(
        menu.provider_current,
        Some(0),
        "● marks the current provider default"
    );
    let core = menu.provider_picker();
    assert_eq!(
        core.items.len(),
        4,
        "default + built-in presets (codex/opencode-go) + deepseek"
    );

    // Number jump 2 = codex (unified order: default → built-in preset → user-defined);
    // level one consumes it, never reaching the input.
    assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
    let menu = chat.model_menu.as_ref().expect("menu is open");
    assert_eq!(
        menu.provider_selected, 1,
        "2 jumps to codex (presets come before user providers)"
    );
    assert_eq!(chat.input, "", "level-one digits are consumed by the menu");

    // Enter into level two: digits are consumed too (out-of-range is swallowed; nothing leaks into the input).
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    let menu = chat.model_menu.as_ref().expect("menu is open");
    assert!(menu.models.is_some(), "enters level two");
    assert!(chat.on_key(KeyCode::Char('3'), KeyModifiers::empty()));
    assert_eq!(
        chat.input, "",
        "level-two digits are consumed by the menu; none leak into the input"
    );
}

/// /model: with an arg, switch the runtime model (effective next turn) and persist as default; without, open the selector.
#[test]
fn slash_model_switches_runtime_model() {
    let home = std::env::temp_dir().join(format!("bingo-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut chat = test_chat_home(home.clone());
    chat.input = "/model deepseek-v4".to_string();
    chat.submit();
    assert_eq!(*chat.session.runtime.model.borrow(), "deepseek-v4");
    assert_eq!(chat.conv.context_usage.window, 1_000_000);
    assert_eq!(chat.conv.context_usage.used, 0);
    assert!(chat.slash_lines.join("\n").contains("deepseek-v4"));
    // No layer defines `model` → the USER layer gets it; the cwd stays
    // untouched (no conjured .bingo/ in arbitrary directories).
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".config/bingo/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["model"], "deepseek-v4",
        "the selection writes back to user settings"
    );
    assert!(
        !home.join(".bingo").exists(),
        "must not create a project layer out of thin air"
    );
    chat.input = "/model".to_string();
    chat.submit();
    assert!(chat.model_menu.is_some(), "no argument enters the selector");
    let _ = std::fs::remove_dir_all(&home);
}

/// /exit sets the quit flag (component layer consumes → system.exit).
#[test]
fn slash_exit_requests_shutdown() {
    let mut chat = test_chat();
    chat.input = "/exit".to_string();
    chat.submit();
    assert!(chat.exit);
}

/// /clear: clears the UI messages and swaps in a new transcript (task keys stay per-session; M0 does not follow).
#[test]
fn slash_clear_resets_session() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::User, "hi"));
    chat.conv.context_usage = crate::context_usage::ContextUsage::new(90_000, 200_000, 160_000);
    chat.input = "/clear".to_string();
    chat.submit();
    assert!(chat.conv.messages.is_empty(), "UI messages cleared");
    assert_eq!(chat.conv.context_usage.used, 0);
    assert!(
        chat.session.runtime.transcript.borrow().is_some(),
        "new transcript"
    );
}

/// /theme: rebuilds the theme (dark → light render difference) + persists to .bingo/settings.json.
#[test]
fn slash_theme_switches_and_persists() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-theme", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    let dark_text = chat.theme.text;
    chat.input = "/theme light".to_string();
    chat.submit();
    assert_ne!(chat.theme.text, dark_text, "theme switched");
    // theme is a user-level preference: with no layer defining it, write the user layer — never create .bingo in cwd.
    let saved = std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap();
    assert!(saved.contains("\"theme\": \"light\""), "{saved}");
    assert!(
        !tmp.join(".bingo").exists(),
        "must not create a project layer out of thin air"
    );

    // When the project layer defines theme: write the effective layer (project), the user layer is no longer bypassed.
    std::fs::create_dir_all(tmp.join(".bingo")).unwrap();
    std::fs::write(
        tmp.join(".bingo/settings.json"),
        "{\n  \"theme\": \"light\"\n}\n",
    )
    .unwrap();
    chat.input = "/theme dark".to_string();
    chat.submit();
    let project = std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap();
    assert!(project.contains("\"theme\": \"dark\""), "{project}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `/theme` with no argument → opens the level selector (picker-model.md commit B): preselects the current level,
/// ↑↓/1-3 browses, Enter applies + persists, Esc cancels without changing state; the `/theme auto` shortcut stays.
#[test]
fn theme_picker_selects_and_applies() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-theme-picker", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();

    // No argument → the menu opens, preselecting the current level (default Auto = index 2).
    chat.input = "/theme".to_string();
    chat.submit();
    let menu = chat.theme_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::theme_levels()[menu.selected].0,
        "auto",
        "preselects the current level auto"
    );
    assert_eq!(
        crate::tui::chat::theme_levels()[menu.current].0,
        "auto",
        "● marks the current level"
    );
    // Esc cancels: state unchanged, menu closed.
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.theme_menu.is_none(), "Esc closes the menu");
    assert_eq!(
        chat.theme_setting,
        crate::tui::theme::ThemeSetting::Auto,
        "Esc does not change the theme"
    );
    assert!(
        !tmp.join(".bingo/settings.json").exists(),
        "cancelling does not write settings"
    );

    // Number jump 2 = light; Enter applies + persists + closes.
    chat.input = "/theme".to_string();
    chat.submit();
    assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
    let menu = chat.theme_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::theme_levels()[menu.selected].0,
        "light",
        "2 jumps to light"
    );
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert!(chat.theme_menu.is_none(), "Enter closes the menu");
    assert_eq!(
        chat.theme_setting,
        crate::tui::theme::ThemeSetting::Light,
        "Enter applies the theme"
    );
    let saved = std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap();
    assert!(saved.contains("\"theme\": \"light\""), "{saved}");

    // ↑↓ browsing and number jump share the core; reopening shows ● on the new level.
    chat.input = "/theme".to_string();
    chat.submit();
    let menu = chat.theme_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::theme_levels()[menu.current].0,
        "light",
        "● follows the active level"
    );
    assert!(chat.on_key(KeyCode::Up, KeyModifiers::empty()));
    let menu = chat.theme_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::theme_levels()[menu.selected].0,
        "dark",
        "up to dark"
    );
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));

    // The shortcut path stays: /theme auto switches directly.
    chat.input = "/theme auto".to_string();
    chat.submit();
    assert_eq!(
        chat.theme_setting,
        crate::tui::theme::ThemeSetting::Auto,
        "the explicit shortcut stays"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /rename: renames the transcript file and updates the runtime reference.
#[test]
fn slash_rename_renames_transcript() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-rename", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let t = crate::transcript::create(&home, &tmp).unwrap();
    // create only makes the directory; drop a message first so the file exists.
    let _ = t.append(&crate::api::types::Message::user_text("hi"));
    let old_name = t.name();
    let task_store = crate::tasks::TaskStore::new(&home, &old_name);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(task_store.create(&crate::tasks::Task {
        id: String::new(),
        subject: "rename task".into(),
        description: String::new(),
        active_form: None,
        status: crate::tasks::TaskStatus::Pending,
        owner: None,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
        metadata: Default::default(),
    }))
    .unwrap();
    let mut chat = test_chat_home(home.clone());
    let _ = chat.session.runtime.transcript_tx.send(Some(t));
    chat.session.tasks.rebind(&old_name);
    chat.input = "/rename my-session".to_string();
    chat.submit();
    let t = chat.session.runtime.transcript.borrow().clone().unwrap();
    assert!(t.name().contains("my-session"), "{}", t.name());
    assert!(t.path().exists());
    assert_eq!(
        chat.tasks()[0].text,
        "rename task",
        "renaming the transcript migrates its task list to the renamed key"
    );
    assert_eq!(
        crate::tasks::TaskStore::new(&home, &t.name()).list_ui()[0].subject,
        "rename task",
        "the renamed task list is restored by its new session key"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /resume: without args, list all sessions; with an arg, switch the runtime transcript by keyword.
#[test]
fn slash_resume_lists_and_switches() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-resume", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let t_a = crate::transcript::create(&home, &tmp).unwrap();
    let _ = t_a.append(&crate::api::types::Message::user_text("aaaa"));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let t_b = crate::transcript::create(&home, &tmp).unwrap();
    let _ = t_b.append(&crate::api::types::Message::user_text("bbbb"));
    let mut chat = test_chat_home(home.clone());
    let _ = chat.session.runtime.transcript_tx.send(Some(t_a.clone()));
    let name_b = t_b.name();
    chat.input = "/resume".to_string();
    chat.submit();
    // No argument → opens the session selector (picker-model.md commit C): the list has b, ● marks the current a.
    let menu = chat.resume_menu.as_ref().expect("selector is open");
    let core = menu.picker();
    assert!(
        core.items.iter().any(|i| i.label == name_b),
        "the selector lists session b"
    );
    assert_eq!(
        menu.current,
        Some(1),
        "● marks the current session (t_a is older, at index 1)"
    );
    // Esc cancels: the current session stays.
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.resume_menu.is_none());
    assert_eq!(
        chat.session
            .runtime
            .transcript
            .borrow()
            .clone()
            .unwrap()
            .name(),
        t_a.name(),
        "Esc does not switch"
    );

    // Enter confirms (snapshot taken by the selected index, the value≠label anchor): switches to b.
    chat.input = "/resume".to_string();
    chat.submit();
    chat.input = String::new();
    chat.on_key(KeyCode::Char('1'), KeyModifiers::empty());
    let menu = chat.resume_menu.as_ref().expect("selector is open");
    assert_eq!(menu.selected, 0, "1 jumps to the newest session");
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert!(chat.resume_menu.is_none(), "Enter closes the selector");
    let current = chat.session.runtime.transcript.borrow().clone().unwrap();
    assert_eq!(
        current.name(),
        name_b,
        "Enter switches the session (snapshot by index)"
    );
    assert!(
        chat.tasks().is_empty(),
        "session b starts with its own tasks"
    );
    assert!(
        chat.conv.context_usage.used > 0,
        "resumed history is estimated"
    );

    // The argument fast path stays.
    chat.input = format!("/resume {}", t_a.name());
    chat.submit();
    let current = chat.session.runtime.transcript.borrow().clone().unwrap();
    assert_eq!(current.name(), t_a.name(), "the argument fast switch stays");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /share: by default exports the current session's HTML locally only
/// (file exists, path echoed, overwrite hint).
#[test]
fn slash_share_exports_current_session_locally_by_default() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-share", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
    let _ = t.append(&crate::api::types::Message::user_text("hi"));
    let mut chat = test_chat_home(home.clone());
    let _ = chat.session.runtime.transcript_tx.send(Some(t));
    chat.input = "/share".to_string();
    chat.submit();
    let stem = chat
        .session
        .runtime
        .transcript
        .borrow()
        .clone()
        .unwrap()
        .name();
    let joined = chat.slash_info_lines.join("\n");
    assert!(joined.contains("exported"), "{joined}");
    assert!(
        joined.contains(&stem),
        "the path contains the stem: {joined}"
    );
    assert!(
        joined.contains("note: this file contains the full conversation"),
        "privacy warning"
    );
    // Output dir = chat.cwd (test_chat_home is set to home).
    let out = home.join(format!("{stem}.html"));
    assert!(out.exists(), "artifact exists: {}", out.display());
    let html = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        html.contains("hi"),
        "the artifact contains the message text"
    );
    assert!(
        html.contains("data-view=\"conv\""),
        "the artifact is a share page"
    );
    // Second export → overwrite notice.
    chat.input = "/share".to_string();
    chat.submit();
    assert!(
        chat.slash_info_lines.join("\n").contains("overwritten"),
        "overwrite notice: {}",
        chat.slash_info_lines.join("\n")
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /share: without a transcript (the new session was never persisted), report that nothing can be exported.
#[test]
fn slash_share_without_transcript_hints() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-noshare", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.input = "/share".to_string();
    chat.submit();
    assert!(
        chat.slash_lines
            .join("\n")
            .contains("no session to export yet"),
        "{}",
        chat.slash_lines.join("\n")
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /share flag parsing (pure logic; no browser or network side effects).
#[test]
fn parse_share_arg_flags() {
    assert!(parse_share_arg("--open", "--open"));
    assert!(parse_share_arg("--public --open", "--open"));
    assert!(parse_share_arg("  --public  ", "--public"));
    assert!(!parse_share_arg("", "--public"));
    assert!(!parse_share_arg("--open", "--public"));
    assert!(!parse_share_arg("--output x", "--public"));
}

/// /share --public: the mock server receives a POST only when public
/// publishing is explicitly chosen; the public-access and sensitive-content
/// warning shows before the upload starts.
#[tokio::test]
async fn slash_share_public_opt_in_warns_before_upload() {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if line.to_ascii_lowercase().starts_with("content-length:") {
                content_length = line
                    .split_once(':')
                    .map(|(_, v)| v.trim().parse().unwrap_or(0))
                    .unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        (request_line, String::from_utf8(body).unwrap())
    });
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-upshare", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
    let _ = t.append(&crate::api::types::Message::user_text("hi"));
    let mut chat = test_chat_home(home.clone());
    // settings.share.baseUrl → a local mock server (runtime session config; no disk/XDG dependency).
    Arc::get_mut(&mut chat.session)
        .unwrap_or_else(|| panic!("single reference"))
        .settings
        .share
        .base_url = Some(format!("http://{addr}"));
    let _ = chat.session.runtime.transcript_tx.send(Some(t));
    chat.input = "/share --public".to_string();
    chat.submit();
    let preflight = chat
        .pinned_panels
        .iter()
        .flat_map(|(_, lines)| lines.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        preflight.contains("anyone can access"),
        "the public scope must be visible before uploading: {preflight}"
    );
    assert!(
        preflight.contains("sensitive information"),
        "the sensitive-content note must be visible before uploading: {preflight}"
    );
    // Wait until the mock server thread finishes (it received the request and replied); stable under parallel load too.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !handle.is_finished() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        handle.is_finished(),
        "the mock server never received the upload request"
    );
    let (request_line, body) = handle.join().unwrap();
    chat.drain_events();
    let joined = chat.slash_info_lines.join("\n");
    assert!(joined.contains("published"), "{joined}");
    assert!(
        joined.contains(&format!("http://{addr}/share/u/")),
        "{joined}"
    );
    assert!(
        chat.pinned_panels.is_empty(),
        "the progress panel clears after the upload completes"
    );
    assert!(request_line.starts_with("POST /share/u/"), "{request_line}");
    assert!(body.contains("hi"), "the uploaded body is the full HTML");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Legacy `--local` remains harmless and resolves to the local-by-default path.
#[test]
fn slash_share_legacy_local_flag_stays_local() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-locshare", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
    let _ = t.append(&crate::api::types::Message::user_text("hi"));
    let mut chat = test_chat_home(home.clone());
    let _ = chat.session.runtime.transcript_tx.send(Some(t));
    chat.input = "/share --local".to_string();
    chat.submit();
    let joined = chat.slash_info_lines.join("\n");
    assert!(joined.contains("exported"), "{joined}");
    assert!(!joined.contains("published"), "local mode does not upload");
    let stem = chat
        .session
        .runtime
        .transcript
        .borrow()
        .clone()
        .unwrap()
        .name();
    assert!(
        home.join(format!("{stem}.html")).exists(),
        "the local file exists"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /permissions: lists rules; adding a rule → runtime table + settings.json persistence.
#[test]
fn slash_permissions_adds_and_lists() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-perms", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat();
    chat.cwd = tmp.display().to_string();
    chat.input = "/permissions".to_string();
    chat.submit();
    assert!(chat.slash_info_lines.join("\n").contains("allow: (none)"));

    chat.input = "/permissions allow Skill(review:*)".to_string();
    chat.submit();
    let rules = chat
        .session
        .runtime
        .permissions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert!(rules.allow.iter().any(|r| r == "Skill(review:*)"));
    let saved = std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap();
    assert!(saved.contains("Skill(review:*)"), "{saved}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /skills: loads and lists the project-level skills directory.
#[test]
fn slash_skills_lists_project_skills() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-skills", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let skill = tmp.join(".bingo/skills/pdf/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\ndescription: Converts documents to PDF\n---\nbody\n",
    )
    .unwrap();
    let mut chat = test_chat();
    chat.cwd = tmp.display().to_string();
    chat.input = "/skills".to_string();
    chat.submit();
    assert!(
        chat.slash_info_lines
            .join("\n")
            .contains("- pdf: Converts documents to PDF"),
        "{}",
        chat.slash_info_lines.join("\n")
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// /tasks: lists the task area (Todo list). Uses a dedicated home to avoid polluting the shared test store.
#[tokio::test]
async fn slash_tasks_lists_todos() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-tasks", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.input = "/tasks".to_string();
    chat.submit();
    let empty = chat.slash_lines.join("\n");
    assert!(empty.contains("no background tasks"), "{empty}");

    let store = chat.session.tasks.clone();
    let id = store
        .create(&crate::tasks::Task {
            id: String::new(),
            subject: "do things".into(),
            description: String::new(),
            active_form: None,
            status: crate::tasks::TaskStatus::Pending,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: Default::default(),
        })
        .await
        .unwrap();
    chat.input = "/tasks".to_string();
    chat.submit();
    let listed = chat.slash_info_lines.join("\n");
    let _ = store.delete(&id).await;
    assert!(listed.contains("do things"), "{listed}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Slash output is transient: rendered after messages and above the input, never settled (not flushed).
#[test]
fn slash_output_rows_render_transient() {
    let mut chat = test_chat();
    chat.input = "/help".to_string();
    chat.submit();
    chat.build_rows(100);
    assert_ne!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "slash output is never settled (not flushed)"
    );
    let joined: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
    assert!(joined.iter().any(|l| l.contains("/model")), "{joined:?}");

    // After the TTL, a tick clears it: the transient hint disappears.
    chat.slash_at =
        Some(std::time::Instant::now() - SLASH_OUTPUT_TTL - std::time::Duration::from_millis(1));
    chat.tick();
    assert!(
        chat.slash_lines.is_empty(),
        "slash output disappears after the timeout"
    );
    assert!(chat.slash_at.is_none());
}

/// Built-in/disk skills submit a `✦ <skill name> [args]` marker via `/skill-name` (progressive disclosure;
/// the model reads the full body on demand via the Skill tool + Read, never into the context).
#[tokio::test]
async fn slash_skill_submits_marker_not_full_content() {
    let mut chat = test_chat();
    chat.input = "/guide".to_string();
    chat.submit();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        chat.drain_all();
        if !chat.conv.busy && !chat.conv.messages.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the skill turn did not end"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        chat.conv.messages[0].text,
        "✦ guide",
        "only the ✦ marker is submitted: {}",
        &chat.conv.messages[0].text[..chat.conv.messages[0].text.len().min(80)]
    );
    assert!(
        !chat.conv.messages[0].text.contains("Diagnostic guide"),
        "the full body no longer enters the context"
    );
}

/// Unknown slash commands still point to /help (no mis-consumption when the skill name does not match).
#[test]
fn slash_unknown_still_guides() {
    let mut chat = test_chat();
    chat.input = "/nope-skill".to_string();
    chat.submit();
    let joined = all_slash_text(&chat);
    assert!(
        joined.contains("unknown command: /nope-skill"),
        "the unknown-command guidance is kept: {joined}"
    );
    assert!(
        joined.contains("code=UNKNOWN_COMMAND"),
        "G13 stable error code: {joined}"
    );
    assert!(
        chat.conv.messages.is_empty(),
        "unknown commands do not start a turn"
    );
}

/// G12 TTL grading: success hints expire after SLASH_OUTPUT_TTL, error/usage rows
/// after SLASH_OUTPUT_ERROR_TTL, and error rows clear on the next input.
#[test]
fn slash_error_rows_have_longer_ttl_and_clear_on_input() {
    let mut chat = test_chat();
    chat.push_slash_output("✓ done".to_string());
    chat.push_slash_error("[error] code=BAD_ARGUMENT msg=usage: /think [...]".to_string());
    assert_eq!(chat.slash_lines.len(), 1);
    assert_eq!(chat.slash_error_lines.len(), 1);

    // Past the success TTL but inside the error TTL: only the success hint expires.
    chat.slash_at =
        Some(std::time::Instant::now() - SLASH_OUTPUT_TTL - std::time::Duration::from_millis(1));
    chat.tick();
    assert!(chat.slash_lines.is_empty(), "success rows expire after 2s");
    assert_eq!(
        chat.slash_error_lines.len(),
        1,
        "error rows have not expired yet"
    );

    // Past the error TTL: both gone.
    chat.slash_error_at = Some(
        std::time::Instant::now() - SLASH_OUTPUT_ERROR_TTL - std::time::Duration::from_millis(1),
    );
    chat.tick();
    assert!(
        chat.slash_error_lines.is_empty(),
        "error rows expire after 8s"
    );

    // A fresh error clears on the next real input edit (after_edit path).
    chat.push_slash_error("usage: /think [...]".to_string());
    assert!(chat.on_key(KeyCode::Char('a'), KeyModifiers::empty()));
    assert!(
        chat.slash_error_lines.is_empty(),
        "the next input clears the error rows"
    );
}

/// G9 no-match hint: a bare `/`-query with zero matches flags the hint; any further
/// keystroke (re-filter) or closing clears it.
#[test]
fn slash_no_match_flags_hint_row() {
    let mut chat = test_chat();
    chat.input = "/zzz".to_string();
    chat.update_slash_suggestions();
    assert!(chat.slash_suggestions.is_empty());
    assert!(chat.slash_no_match, "/zzz with no match shows the hint");
    // Typing further re-filters: still no match, hint stays.
    chat.input = "/zzzx".to_string();
    chat.update_slash_suggestions();
    assert!(chat.slash_no_match);
    // A matching prefix clears it.
    chat.input = "/th".to_string();
    chat.update_slash_suggestions();
    assert!(!chat.slash_suggestions.is_empty());
    assert!(!chat.slash_no_match, "a match suppresses the hint");
    // Clearing the input (any path that re-runs the filter) removes the hint.
    chat.input = "/zzz".to_string();
    chat.update_slash_suggestions();
    assert!(chat.slash_no_match);
    chat.input = String::new();
    chat.update_slash_suggestions();
    assert!(!chat.slash_no_match, "an empty input suppresses the hint");
}

/// P1-E: /provider list keys are redacted — short keys (≤4 chars) get no ellipsis.
#[test]
fn slash_provider_list_masks_short_keys() {
    let mut chat = test_chat();
    let settings = crate::settings::Settings {
        api_key: Some("main".into()),
        ..Default::default()
    };
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings(&settings).unwrap();
    chat.input = "/provider".to_string();
    chat.submit();
    // No argument → opens the selector: the info column (URL + redacted key) goes into desc (picker-model.md commit D).
    let menu = chat.provider_menu.as_ref().expect("selector is open");
    let core = menu.picker();
    let desc = core
        .items
        .iter()
        .find(|i| i.label == "default")
        .map(|i| i.description.as_str())
        .expect("the default option");
    assert!(desc.contains("https://api.anthropic.com"), "{desc}");
    assert!(
        desc.contains("key main"),
        "short keys have no ellipsis: {desc}"
    );
    assert!(!desc.contains("main…"), "{desc}");
}

/// P5 (D34): with empty settings, /provider login codex takes the preset oauth path —
/// it must not say "provider not found" nor "API key required" (codex is an oauth-type preset).
/// (tokio: the device-auth branch spawns on the runtime.)
#[tokio::test]
async fn slash_provider_login_uses_preset_with_empty_settings() {
    let tmp = std::env::temp_dir().join(format!("bingo-preset-login-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.input = "/provider login codex --device-auth".to_string();
    chat.submit();
    let out = all_slash_text(&chat);
    assert!(!out.contains("provider not found"), "{out}");
    assert!(
        !out.contains("requires an API key"),
        "codex is an oauth-type preset: {out}"
    );
    // opencode-go is an apiKey-type preset → without --manual it guides toward a key.
    let mut chat = test_chat_home(tmp.join("home2"));
    chat.input = "/provider login opencode-go".to_string();
    chat.submit();
    let out = all_slash_text(&chat);
    assert!(
        out.contains("requires an API key"),
        "an apiKey-type preset guides toward pasting a key: {out}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// P5: opencode-go --manual stores auth.json `{type:"api"}` (zero settings pollution).
#[tokio::test]
async fn slash_provider_login_opencode_go_manual_stores_api_key() {
    let tmp = std::env::temp_dir().join(format!("bingo-preset-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let mut chat = test_chat_home(home.clone());
    chat.input = "/provider login opencode-go --manual sk-og".to_string();
    chat.submit();
    let store = crate::auth::AuthStore::new(&home);
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if let Ok(Some(entry)) = store.get("opencode-go") {
            match entry {
                crate::auth::AuthEntry::Api { key } => {
                    assert_eq!(key, "sk-og", "opencode-go key storage");
                }
                other => panic!("expected an Api entry: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    panic!("opencode-go key was not stored");
}

/// P4: switching to a not-logged-in OAuth provider → the switch succeeds but carries a login hint; the list
/// shows the protocol marker + the not-logged-in state, and a missing apiBaseUrl → the protocol default endpoint.
#[test]
fn slash_provider_switch_warns_on_oauth_not_logged_in() {
    let tmp = std::env::temp_dir().join(format!("bingo-preset-warn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    let mut settings = crate::settings::Settings {
        api_key: Some("sk-main".into()),
        ..Default::default()
    };
    settings.providers.insert(
        "codex".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: None,
            api_base_url: String::new(),
            supports_images: None,
            protocol: Some("openai".into()),
            oauth: Some(crate::settings::OauthConfig {
                kind: "codex".into(),
                account: None,
            }),
        },
    );
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings_at_with(
            &settings,
            |_| Err(std::env::VarError::NotPresent),
            &tmp.join("home"),
        )
        .unwrap();
    chat.input = "/provider codex".to_string();
    chat.submit();
    let out = chat.slash_lines.join("\n");
    assert!(out.contains("✓ provider switched: codex"), "{out}");
    assert!(
        out.contains("not logged in: /provider login codex"),
        "not-logged-in hint: {out}"
    );

    chat.input = "/provider".to_string();
    chat.submit();
    // No argument → selector: desc carries the preset endpoint + not-logged-in state + protocol + built-in badge.
    let menu = chat.provider_menu.as_ref().expect("selector is open");
    let core = menu.picker();
    let desc = core
        .items
        .iter()
        .find(|i| i.label == "codex")
        .map(|i| i.description.as_str())
        .expect("the codex option");
    assert!(
        desc.contains("https://chatgpt.com/backend-api"),
        "missing apiBaseUrl → preset endpoint: {desc}"
    );
    assert!(
        desc.contains("○ not logged in (/provider login codex) · openai · built-in"),
        "protocol marker + not-logged-in state + built-in: {desc}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// P4: turn-level error text gains auth hints — an oauth provider 401 → a login hint (with the name);
/// non-oauth 401 gets none (apiKey scenarios have no login concept); 403 → a model/subscription hint;
/// hints already present / unrelated codes pass through verbatim.
#[test]
fn auth_error_hint_appends_login_guidance() {
    let base = "API error: HTTP 401: invalid token".to_string();
    let hinted = auth_hint_for(true, "codex", "AUTH_REQUIRED", base.clone());
    assert!(
        hinted.contains("/provider login codex"),
        "the oauth 401 hint carries the provider name: {hinted}"
    );

    // A non-oauth provider's 401: no login hint (an expired apiKey just needs the key checked).
    let api_key_msg = auth_hint_for(false, "deepseek", "AUTH_REQUIRED", base.clone());
    assert_eq!(api_key_msg, base, "non-oauth 401 passes through verbatim");

    // An AuthError that already carries a login hint (permanent refresh failure) is not extended again.
    let already =
        "login has expired (refresh_token_expired): /provider login to sign in again".to_string();
    assert_eq!(
        auth_hint_for(true, "codex", "AUTH_REQUIRED", already.clone()),
        already,
        "an existing hint is not repeated"
    );

    // 403 → a model/subscription hint.
    let denied = auth_hint_for(
        false,
        "deepseek",
        "PERMISSION_DENIED",
        "API error: HTTP 403: quota".into(),
    );
    assert!(
        denied.contains("/model"),
        "the 403 hints at /model: {denied}"
    );

    // Unrelated codes pass through verbatim.
    let rate = "API error: HTTP 429: rate limited".to_string();
    assert_eq!(
        auth_hint_for(true, "codex", "RATE_LIMITED", rate.clone()),
        rate
    );
}

#[test]
fn slash_provider_lists_and_switches() {
    let s_tmp = std::env::temp_dir().join(format!("bingo-slash-{}-provs", std::process::id()));
    let _ = std::fs::remove_dir_all(&s_tmp);
    let mut chat = test_chat();
    chat.input = "/provider".to_string();
    chat.submit();
    // No argument → opens the selector: default first, ● marks the current.
    let menu = chat.provider_menu.as_ref().expect("selector is open");
    assert_eq!(menu.current, Some(0), "● marks the current default");
    let core = menu.picker();
    assert_eq!(
        core.items.first().map(|i| i.label.as_str()),
        Some("default"),
        "default comes first"
    );

    // Configure a named provider, then switch.
    let providers = std::collections::HashMap::from([(
        "deepseek".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: Some("sk-ds".into()),
            api_base_url: "https://api.deepseek.com".into(),
            supports_images: None,
            protocol: None,
            oauth: None,
        },
    )]);
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::new("sk-main".into(), "https://main.example".into());
    // set_provider needs a providers table — constructing via from_settings is more direct.
    drop(providers);
    let mut settings = crate::settings::Settings {
        api_key: Some("sk-main".into()),
        ..Default::default()
    };
    settings.providers.insert(
        "deepseek".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: Some("sk-ds".into()),
            api_base_url: "https://api.deepseek.com".into(),
            supports_images: None,
            protocol: None,
            oauth: None,
        },
    );
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings(&settings).unwrap();

    // Reopen the selector: the list has deepseek; Esc does not change the current.
    chat.input = "/provider".to_string();
    chat.submit();
    let menu = chat.provider_menu.as_ref().expect("selector is open");
    let core = menu.picker();
    assert!(
        core.items.iter().any(|i| i.label == "deepseek"),
        "the selector lists deepseek"
    );
    assert_eq!(menu.current, Some(0), "● marks the current default");
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.provider_menu.is_none(), "Esc closes the selector");
    assert_eq!(
        *chat.session.runtime.provider.borrow(),
        "default",
        "Esc does not change"
    );

    // Enter confirms: switch + persist (equivalent to the argument fast path).
    chat.input = "/provider".to_string();
    chat.submit();
    // Order: default(1) → codex(2) → opencode-go(3) → deepseek(4).
    assert!(chat.on_key(KeyCode::Char('4'), KeyModifiers::empty()));
    let menu = chat.provider_menu.as_ref().expect("selector is open");
    assert_eq!(menu.selected, 3, "4 jumps to deepseek");
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert!(chat.provider_menu.is_none(), "Enter closes the selector");
    assert_eq!(
        *chat.session.runtime.provider.borrow(),
        "deepseek",
        "the runtime provider is synced"
    );
    let out = chat.slash_lines.join("\n");
    assert!(out.contains("✓ provider switched: deepseek"), "{out}");

    // The argument fast path stays.
    chat.input = "/provider deepseek".to_string();
    chat.submit();
    assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");
    let _ = std::fs::remove_dir_all(&s_tmp);

    // s = this session only (a fresh chat, nothing persisted before): runtime switch without writing settings.
    let mut chat = test_chat_home(s_tmp.join("home"));
    chat.cwd = s_tmp.display().to_string();
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings(&settings).unwrap();
    chat.input = "/provider".to_string();
    chat.submit();
    let menu = chat.provider_menu.as_ref().expect("selector is open");
    assert_eq!(menu.current, Some(0), "● marks the current default");
    assert!(chat.on_key(KeyCode::Char('4'), KeyModifiers::empty()));
    assert!(chat.on_key(KeyCode::Char('s'), KeyModifiers::empty()));
    assert_eq!(
        *chat.session.runtime.provider.borrow(),
        "deepseek",
        "s switches the runtime"
    );
    let out = chat.slash_lines.join("\n");
    assert!(
        out.contains("(this session only)"),
        "the s annotation: {out}"
    );
    assert!(
        !s_tmp.join(".bingo/settings.json").exists(),
        "s does not write settings"
    );

    // A miss errors (into the error bucket).
    chat.input = "/provider nope".to_string();
    chat.submit();
    let out = all_slash_text(&chat);
    assert!(out.contains("not found"), "{out}");
}

#[test]
fn slash_think_sets_level_and_persists() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-think", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();

    // No arg → open the level selector (preselects off = first item).
    chat.input = "/think".to_string();
    chat.submit();
    let menu = chat.think_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::think_levels()[menu.selected].0,
        "off",
        "preselects off when unset"
    );
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.think_menu.is_none(), "Esc exits the menu");

    // New level xhigh: runtime effect + persistence.
    chat.input = "/think xhigh".to_string();
    chat.submit();
    let out = chat.slash_lines.join("\n");
    assert!(out.contains("✓ thinking level set: xhigh"), "{out}");
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("xhigh")
    );
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["thinkingLevel"], "xhigh");

    chat.input = "/think off".to_string();
    chat.submit();
    let out = chat.slash_lines.join("\n");
    assert!(out.contains("✓ thinking level set: off"), "{out}");
    assert_eq!(chat.session.runtime.thinking.borrow().as_deref(), None);

    chat.input = "/think bogus".to_string();
    chat.submit();
    let out = chat.slash_error_lines.join("\n");
    assert!(
        out.contains("usage: /think") && out.contains("code=BAD_ARGUMENT"),
        "the usage line carries a stable error code: {out}"
    );
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        None,
        "an invalid argument does not change the state"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ------------------------------------------------------------------
// /mcp: list / enable|disable (persisted list) / reconnect
// ------------------------------------------------------------------

async fn slash_mcp_wait(chat: &mut Chat) -> String {
    // Results land in whichever tier fits (confirm/info/error); only NEW
    // lines count — info/error persist across steps by design.
    let start = chat.slash_lines.len();
    let start_info = chat.slash_info_lines.len();
    let start_err = chat.slash_error_lines.len();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        chat.drain_all();
        let output: Vec<String> = chat.slash_lines[start.min(chat.slash_lines.len())..]
            .iter()
            .chain(&chat.slash_info_lines[start_info.min(chat.slash_info_lines.len())..])
            .chain(&chat.slash_error_lines[start_err.min(chat.slash_error_lines.len())..])
            .filter(|l| !l.starts_with('⏳'))
            .map(|l| l.to_string())
            .collect();
        if !output.is_empty() {
            return output.join("\n");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "slash output timed out"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn slash_mcp_lists_unconfigured() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-mcp1", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    chat.input = "/mcp".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("no MCP servers configured"), "{out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn slash_mcp_enable_disable_persists_and_lists() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-mcp2", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    Arc::get_mut(&mut chat.session).unwrap().runtime.mcp =
        Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
            std::collections::HashMap::from([(
                "files".to_string(),
                crate::settings::McpServerConfig {
                    kind: None,
                    command: Some("/bin/echo".to_string()),
                    args: Vec::new(),
                    env: Default::default(),
                    url: None,
                    headers: Default::default(),
                },
            )]),
            Default::default(),
        )));
    chat.input = "/mcp".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("MCP servers (1)"), "{out}");
    assert!(out.contains("files"), "{out}");

    chat.input = "/mcp disable files".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("disabled 1 MCP server(s): files"), "{out}");
    // Persisted to .bingo/settings.json
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap())
            .unwrap();
    assert_eq!(saved["disabledMcpServers"], serde_json::json!(["files"]));
    // The list shows disabled
    chat.input = "/mcp".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("files  disabled"), "{out}");

    chat.input = "/mcp enable all".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("enabled 1 MCP server(s): files"), "{out}");
    chat.input = "/mcp".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(!out.contains("disabled"), "{out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn slash_mcp_reconnect_unknown_server() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-mcp3", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    Arc::get_mut(&mut chat.session).unwrap().runtime.mcp =
        Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
            std::collections::HashMap::from([(
                "files".to_string(),
                crate::settings::McpServerConfig {
                    kind: None,
                    command: Some("/bin/echo".to_string()),
                    args: Vec::new(),
                    env: Default::default(),
                    url: None,
                    headers: Default::default(),
                },
            )]),
            Default::default(),
        )));
    chat.input = "/mcp reconnect nope".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("no MCP server \"nope\""), "{out}");
    // Reconnect a failing server: the failure detail shows through
    chat.input = "/mcp reconnect files".to_string();
    chat.submit();
    let out = slash_mcp_wait(&mut chat).await;
    assert!(out.contains("files"), "{out}");
    assert!(
        out.contains("handshake failed") || out.contains("✗"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ------------------------------------------------------------------
// Slash dropdown suggestions (pop up on /; Tab completes / ↑↓ navigate / Enter runs / Esc closes)
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// Slash dropdown suggestions (pop up on /; Tab completes / ↑↓ navigate / Enter runs / Esc closes)
// ------------------------------------------------------------------

/// Typing `/` → suggestions list the built-in commands; gone once args follow.
#[test]
fn slash_menu_lists_commands_and_hides_with_args() {
    let mut chat = test_chat();
    chat.input = "/".to_string();
    chat.update_slash_suggestions();
    assert!(
        chat.slash_suggestions.len() >= crate::app::action::COMMANDS.len(),
        "everything lands in state (including skill expansion; the render layer windows around the selection, so commands 6+ are reachable again)"
    );
    assert!(chat.slash_suggestions.iter().any(|s| s.name == "model"));
    // Render-layer window: 5 visible + a "N more" indicator.
    let rows = crate::tui::el::render(crate::tui::chrome::chrome(&chat, 100, false)).rows;
    let joined: String = rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("more"),
        "the out-of-window count is visible: {joined}"
    );

    chat.input = "/model deepseek".to_string();
    chat.update_slash_suggestions();
    assert!(
        chat.slash_suggestions.is_empty(),
        "no suggestions with an argument"
    );

    chat.input = "hi".to_string();
    chat.update_slash_suggestions();
    assert!(
        chat.slash_suggestions.is_empty(),
        "no suggestions without a leading /"
    );
}

/// Dispatch completeness: every command in the table is reachable through
/// `run_slash`, and the terminal invents no name the table does not have.
///
/// The hand-kept mirror of `run_slash`'s match arms that used to live here is
/// gone with the arms (D146): dispatch is a match on the parsed `Command`, so
/// the compiler is what keeps it exhaustive, and the table is what decides
/// which names exist.
#[test]
fn slash_dispatch_covers_every_table_entry() {
    for spec in crate::app::action::COMMANDS {
        for name in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
            let read = crate::app::action::parse(name);
            assert!(
                !matches!(read, Err(crate::app::action::ParseError::Unknown(_))),
                "the registered command /{name} does not answer to its own name"
            );
        }
    }
    let mut chat = test_chat();
    chat.run_slash("nonesuch");
    assert!(
        chat.slash_error_lines
            .join("\n")
            .contains(crate::error::SLASH_ERROR_UNKNOWN_COMMAND),
        "a name the table does not have is refused by name"
    );
}

/// `/help` renders every table entry (title + one line each) with its hint,
/// straight from the same table — the single source stays the only source.
#[test]
fn slash_help_lists_every_command_with_hint() {
    let mut chat = test_chat();
    chat.run_slash("help");
    let lines: Vec<&str> = chat.slash_info_lines.iter().map(String::as_str).collect();
    assert_eq!(
        lines.len(),
        crate::app::action::COMMANDS.len() + 2,
        "title + one row per command + key cross-link"
    );
    assert_eq!(lines[0], "available commands:");
    assert!(
        lines.iter().any(|l| l.contains("login")),
        "sub-commands are discoverable, and from the table's own hint now"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("press ? on an empty input")),
        "cross-linked to the ? panel"
    );
    for (spec, line) in crate::app::action::COMMANDS.iter().zip(&lines[1..]) {
        assert!(
            line.contains(&spec.usage()),
            "the row carries the command and its argument hint: {line}"
        );
        assert!(
            line.ends_with(spec.description),
            "the row ends with the description: {line}"
        );
    }
}

/// Prefix filtering + skills merged in (project-level skills directory).
#[test]
fn slash_menu_filters_by_prefix_and_includes_skills() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-menu", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let skill = tmp.join(".bingo/skills/pdf/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, "---\ndescription: PDF tool\n---\nbody\n").unwrap();

    let mut chat = test_chat();
    chat.cwd = tmp.display().to_string();
    chat.input = "/p".to_string();
    chat.update_slash_suggestions();
    assert!(
        chat.slash_suggestions.iter().any(|s| s.name == "pdf"),
        "skills merge into the suggestions"
    );

    chat.input = "/mo".to_string();
    chat.update_slash_suggestions();
    let names: Vec<&str> = chat
        .slash_suggestions
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["model"], "prefix filtering: {names:?}");

    // Overlong descriptions are truncated (MAX_LISTING_DESC_CHARS):
    // a NoWrap overlong row would push the canvas past the terminal width → stale diff residue.
    let long = "x".repeat(400);
    std::fs::write(&skill, format!("---\ndescription: {long}\n---\nbody\n")).unwrap();
    chat.input = "/p".to_string();
    chat.update_slash_suggestions();
    let desc = chat
        .slash_suggestions
        .iter()
        .find(|s| s.name == "pdf")
        .map(|s| s.description.clone())
        .expect("the pdf skill is among the suggestions");
    assert!(
        desc.chars().count() <= crate::skills::MAX_LISTING_DESC_CHARS,
        "description truncated: {} chars",
        desc.chars().count()
    );
    assert!(
        desc.ends_with('…'),
        "truncation carries an ellipsis: {desc}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// ↑/↓ move the selection (keys consumed, no scroll); Tab completes `/name ` without running it.
#[test]
fn slash_menu_navigation_and_tab_completion() {
    let mut chat = test_chat();
    chat.input = "/".to_string();
    chat.update_slash_suggestions();
    assert_eq!(chat.slash_selected, 0);

    assert!(chat.slash_menu_key(KeyCode::Down, KeyModifiers::empty()));
    assert_eq!(chat.slash_selected, 1);
    assert!(chat.slash_menu_key(KeyCode::Up, KeyModifiers::empty()));
    assert_eq!(chat.slash_selected, 0);
    assert!(chat.slash_menu_key(KeyCode::Up, KeyModifiers::empty()));
    assert_eq!(
        chat.slash_selected,
        chat.slash_suggestions.len() - 1,
        "wraps at the top"
    );

    // Tab applies the selection (/help) → `/help ` with suggestions cleared and nothing run.
    chat.input = "/".to_string();
    chat.update_slash_suggestions();
    chat.slash_selected = 0;
    assert!(chat.slash_menu_key(KeyCode::Tab, KeyModifiers::empty()));
    assert_eq!(chat.input, "/help ");
    assert!(chat.slash_suggestions.is_empty());
    assert!(chat.slash_lines.is_empty(), "Tab does not execute");

    // Esc closes.
    chat.input = "/".to_string();
    chat.update_slash_suggestions();
    assert!(chat.slash_menu_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.slash_suggestions.is_empty());
}

/// Enter: partial prefix → apply the selection and run; full command → run as-is.
#[tokio::test]
async fn slash_menu_enter_applies_and_executes() {
    let mut chat = test_chat();
    // Full command: run directly; the suggestion menu must close (no leftover placeholder row).
    chat.input = "/model".to_string();
    chat.update_slash_suggestions();
    assert!(
        !chat.slash_suggestions.is_empty(),
        "typing /model shows suggestions: {:?}",
        chat.slash_suggestions
    );
    chat.submit();
    assert!(
        chat.model_menu.is_some(),
        "/model enters the two-level selector (level one = the endpoint list)"
    );
    assert!(
        chat.slash_suggestions.is_empty(),
        "menu mode has no slash suggestions"
    );
    assert!(!chat.conv.busy);
    // Esc exits the menu.
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.model_menu.is_none(), "Esc exits the menu");

    // Partial prefix `/sta`: Enter applies the selection (status first) and runs it.
    chat.input = "/sta".to_string();
    chat.update_slash_suggestions();
    assert!(
        chat.slash_suggestions.iter().any(|s| s.name == "status"),
        "has suggestions: {:?}",
        chat.slash_suggestions
    );
    chat.submit();
    assert!(
        chat.pinned_panels
            .iter()
            .any(|(_, l)| l.join("").contains("⏳")),
        "status ran (async stats hint)"
    );
    assert!(
        chat.slash_suggestions.is_empty(),
        "the menu closes after a partial-prefix execution"
    );
}

/// `/model` two-level selector: Enter opens the menu (level-one endpoint list),
/// move the selection → Enter goes to level two (loading) → Esc exits level by level.
#[tokio::test]
async fn model_menu_two_stage_navigation() {
    let mut chat = test_chat();
    chat.input = "/model".to_string();
    chat.submit();
    let Some(menu) = &chat.model_menu else {
        panic!("menu did not open");
    };
    assert_eq!(
        menu.providers,
        vec!["default"],
        "the level-one list contains the current endpoint"
    );
    assert!(menu.models.is_none(), "stops at level one");
    assert!(
        chat.on_key(KeyCode::Down, KeyModifiers::empty()),
        "↓ moves the selection"
    );
    assert_eq!(
        chat.model_menu.as_ref().unwrap().provider_selected,
        0,
        "a single-item list wraps back to 0"
    );
    // Enter goes to level two: async fetch in progress (loading).
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    let m = &chat.model_menu.as_ref().unwrap().models;
    assert!(m.is_some(), "entered level two");
    assert!(m.as_ref().unwrap().loading, "fetching");
    // Esc returns level by level: two → one → exit.
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(
        chat.model_menu.as_ref().is_some_and(|m| m.models.is_none()),
        "level-two Esc returns to level one"
    );
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.model_menu.is_none(), "level-one Esc exits entirely");
}

/// Level-two confirm: the model is picked → switch the runtime model and exit the menu.
#[tokio::test]
async fn model_menu_picks_model_and_switches() {
    let mut chat = test_chat();
    chat.input = "/model".to_string();
    chat.submit();
    chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    if let Some(m) = &mut chat.model_menu.as_mut().unwrap().models {
        m.models = vec![
            "deepseek-v4".to_string().into(),
            "deepseek-r1".to_string().into(),
        ];
        m.loading = false;
        m.selected = 1;
    }
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert_eq!(
        *chat.session.runtime.model.borrow(),
        "deepseek-r1",
        "the selected model takes effect"
    );
    assert!(
        chat.model_menu.is_none(),
        "closes the menu after confirming"
    );
    assert!(
        chat.slash_lines.join("\n").contains("model switched"),
        "confirmation hint"
    );
}

/// With multiple providers, level-two Esc returns to level one: the level-one provider list and selection must survive
/// (regression: open_model_models used to rebuild providers as a single element, losing the list after Esc).
#[tokio::test]
async fn model_menu_esc_back_keeps_provider_list() {
    let home = std::env::temp_dir().join(format!("bingo-model-esc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut chat = test_chat_home(home.clone());
    let mut settings = crate::settings::Settings {
        api_key: Some("sk-main".into()),
        ..Default::default()
    };
    for (name, key, url) in [
        ("deepseek", "sk-ds", "https://api.deepseek.com"),
        ("local", "sk-local", "http://127.0.0.1:11434"),
    ] {
        settings.providers.insert(
            name.to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some(key.into()),
                api_base_url: url.into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
    }
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings(&settings).unwrap();

    chat.input = "/model".to_string();
    chat.submit();
    let providers = chat.model_menu.as_ref().unwrap().providers.clone();
    assert_eq!(
        providers,
        vec!["default", "codex", "opencode-go", "deepseek", "local"],
        "the same order as /provider: default → built-in preset → user-defined"
    );
    assert_eq!(chat.model_menu.as_ref().unwrap().provider_selected, 0);

    // ↓ twice selects local → Enter into level two (loading) → Esc back to level one.
    chat.on_key(KeyCode::Down, KeyModifiers::empty());
    chat.on_key(KeyCode::Down, KeyModifiers::empty());
    assert_eq!(chat.model_menu.as_ref().unwrap().provider_selected, 2);
    chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    assert!(
        chat.model_menu.as_ref().unwrap().models.is_some(),
        "enters level two"
    );
    chat.on_key(KeyCode::Esc, KeyModifiers::empty());
    let menu = chat.model_menu.as_ref().expect("level one is still there");
    assert_eq!(
        menu.providers,
        vec!["default", "codex", "opencode-go", "deepseek", "local"],
        "the list is kept"
    );
    assert_eq!(menu.provider_selected, 2, "the selection is kept");
    assert!(menu.models.is_none(), "back at level one");

    let _ = std::fs::remove_dir_all(&home);
}

/// P0-A: /provider <name> persists the provider; confirming in the /model menu
/// writes provider + model together into `.bingo/settings.json` (restart restores the same endpoint and model).
#[tokio::test]
async fn provider_switch_persists_provider_and_model_menu_persists_both() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-provpersist", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    let mut settings = crate::settings::Settings {
        api_key: Some("sk-main".into()),
        ..Default::default()
    };
    settings.providers.insert(
        "deepseek".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: Some("sk-ds".into()),
            api_base_url: "https://api.deepseek.com".into(),
            supports_images: None,
            protocol: None,
            oauth: None,
        },
    );
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings(&settings).unwrap();

    // /provider deepseek: switch + persist.
    chat.input = "/provider deepseek".to_string();
    chat.submit();
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["provider"], "deepseek",
        "the provider is persisted (user layer)"
    );
    assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");

    // /model menu: current provider=deepseek (preselected) → level-two confirms the model
    // → provider + model persist together.
    chat.input = "/model".to_string();
    chat.submit();
    assert_eq!(
        chat.model_menu.as_ref().unwrap().provider_selected,
        3,
        "level one preselects the current provider (unified order: default, codex, opencode-go, deepseek)"
    );
    chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    if let Some(m) = &mut chat.model_menu.as_mut().unwrap().models {
        m.models = vec!["deepseek-v4".to_string().into()];
        m.loading = false;
    }
    chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    assert!(
        chat.model_menu.is_none(),
        "closes the menu after confirming"
    );
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["model"], "deepseek-v4", "the model is persisted");
    assert_eq!(
        saved["provider"], "deepseek",
        "the provider persists with the model"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// P1-E: /model <name> sets directly — with a cache and no hit → a non-blocking hint;
/// no cache / never fetched → switches directly with no hint.
#[test]
fn slash_model_validates_against_cached_list() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-modelval", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.clone());

    // No cache: switch directly, no validation hint.
    chat.input = "/model custom-new".to_string();
    chat.submit();
    let out = chat.slash_lines.join("\n");
    assert!(out.contains("✓ model switched: custom-new"), "{out}");
    assert!(!out.contains("not in"), "{out}");

    // The current provider has a cache and the model is not in it: the success note carries a ⚠ hint,
    // one line (advisory, non-blocking; it still switches).
    chat.slash_lines.clear();
    chat.models_cache.insert(
        "default".to_string(),
        vec!["claude-sonnet-5".to_string(), "deepseek-v4".to_string()],
    );
    chat.input = "/model unknown-xyz".to_string();
    chat.submit();
    let out = chat.slash_lines.join("\n");
    assert!(
        out.contains("✓ model switched: unknown-xyz (⚠ not in default's known list"),
        "{out}"
    );
    assert_eq!(
        out.lines().count(),
        1,
        "one-line output; ⚠ and ✓ do not coexist"
    );
    assert_eq!(*chat.session.runtime.model.borrow(), "unknown-xyz");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// P1-F: after ModelsLoaded, the current provider's current model is preselected (the counterpart of /think
/// preselecting the current level), so browsing cannot accidentally switch; a model missing from the list falls back to 0; the result
/// is written into models_cache (used by /model <name> validation).
#[tokio::test]
async fn models_loaded_preselects_current_model_and_caches() {
    let mut chat = test_chat();
    chat.input = "/model".to_string();
    chat.submit();
    chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    // Current provider=default, current model=test-model (test_chat's initial value).
    chat.apply_event(UiEvent::ModelsLoaded {
        provider: "default".into(),
        models: vec!["m0".into(), "test-model".into(), "m2".into()],
        failed: None,
    });
    let m = chat.model_menu.as_ref().unwrap().models.as_ref().unwrap();
    assert_eq!(m.selected, 1, "preselects the current model");
    assert_eq!(m.models[m.selected].id, "test-model");
    assert_eq!(
        chat.models_cache.get("default").map(Vec::as_slice),
        Some(&["m0".to_string(), "test-model".to_string(), "m2".to_string()][..]),
        "the load result lands in the cache"
    );

    // The current model is not in the list: the selection falls back to 0.
    chat.apply_event(UiEvent::ModelsLoaded {
        provider: "default".into(),
        models: vec!["m0".into(), "m1".into()],
        failed: None,
    });
    let m = chat.model_menu.as_ref().unwrap().models.as_ref().unwrap();
    assert_eq!(m.selected, 0, "a miss falls back to 0");
}

/// /think with no argument enters the level selector: preselects the current level, ↑↓ moves, Enter confirms, Esc exits.
#[test]
fn think_menu_navigates_and_confirms() {
    let home = std::env::temp_dir().join(format!("bingo-think-menu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut chat = test_chat_home(home.clone());
    let _ = chat.session.runtime.thinking_tx.send(Some("high".into()));
    chat.input = "/think".to_string();
    chat.submit();
    let menu = chat.think_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::think_levels()[menu.selected].0,
        "high",
        "preselects the current level"
    );
    // ↑ to medium, Enter confirms: runtime effect + persistence + menu closes.
    assert!(chat.on_key(KeyCode::Up, KeyModifiers::empty()));
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert!(
        chat.think_menu.is_none(),
        "closes the menu after confirming"
    );
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("medium")
    );
    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".config/bingo/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["thinkingLevel"], "medium",
        "the selection is persisted"
    );
    // Reopen the menu: Esc exits directly; off clears the level.
    chat.input = "/think".to_string();
    chat.submit();
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.think_menu.is_none(), "Esc exits");
    chat.input = "/think off".to_string();
    chat.submit();
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        None,
        "off clears the level"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The think vocabulary (selector) matches the API layer's THINKING_LEVELS: off + all levels, in the same order.
#[test]
fn think_levels_match_api_levels() {
    assert_eq!(crate::tui::chat::think_levels()[0].0, "off");
    let menu: Vec<&str> = crate::tui::chat::think_levels()[1..]
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(menu, crate::api::contract::THINKING_LEVELS.to_vec());
}

/// 1..6 direct jump selects the right row and the digits never reach the input;
/// `s` applies session-only (runtime changes, settings.json not written).
#[test]
fn think_menu_direct_jump_and_session_only() {
    let home = std::env::temp_dir().join(format!("bingo-think-menu-s-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut chat = test_chat_home(home.clone());
    let _ = chat.session.runtime.thinking_tx.send(Some("off".into()));
    chat.input = "/think".to_string();
    chat.submit();
    let menu = chat.think_menu.as_ref().expect("menu is open");
    assert_eq!(menu.current, 0, "● records the active level at open time");
    // '3' jumps to medium (off=1, low=2, medium=3); digits are consumed, not typed.
    assert!(chat.on_key(KeyCode::Char('3'), KeyModifiers::empty()));
    let menu = chat.think_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::think_levels()[menu.selected].0,
        "medium",
        "3 jumps to medium"
    );
    assert_eq!(
        chat.input, "",
        "digit keys are consumed by the menu, never reaching the input"
    );
    // '6' wraps-jumps to max; Enter persists.
    assert!(chat.on_key(KeyCode::Char('6'), KeyModifiers::empty()));
    let menu = chat.think_menu.as_ref().expect("menu is open");
    assert_eq!(
        crate::tui::chat::think_levels()[menu.selected].0,
        "max",
        "6 jumps to max"
    );
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("off"),
        "Esc does not change the state"
    );

    // `s`: session-only — runtime switches, no settings write.
    chat.input = "/think".to_string();
    chat.submit();
    assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
    assert!(chat.on_key(KeyCode::Char('s'), KeyModifiers::empty()));
    assert!(
        chat.think_menu.is_none(),
        "s closes the menu after confirming"
    );
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("low"),
        "s switches the runtime"
    );
    let out = chat.slash_lines.join("\n");
    assert!(
        out.contains("(this session only)"),
        "the s output is annotated this-session-only: {out}"
    );
    assert!(
        !home.join(".bingo/settings.json").exists(),
        "s does not write settings.json"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Footer badge: shows `model · think level` when a level is set; off shows only the model name.
#[test]
fn footer_model_label_shows_thinking_level() {
    assert_eq!(
        model_footer_label("deepseek-v4", Some("high")),
        "deepseek-v4 · think high"
    );
    assert_eq!(model_footer_label("deepseek-v4", None), "deepseek-v4");
    assert_eq!(
        model_footer_label("deepseek-v4", Some("off")),
        "deepseek-v4"
    );
}

// ------------------------------------------------------------------
// Collapse classification & summaries (formerly fold_tests)
// ------------------------------------------------------------------

#[test]
fn result_summaries() {
    assert_eq!(
        result_summary("Read", "line1\nline2\n\nline3"),
        Some("Read 3 lines".to_string())
    );
    assert_eq!(
        result_summary("Grep", "a:1:x\nb:2:y"),
        Some("Found 2 matches".to_string())
    );
    assert_eq!(
        result_summary("Glob", "a.rs\nb.rs"),
        Some("Found 2 files".to_string())
    );
    assert_eq!(result_summary("Bash", "out"), None);
}

// ------------------------------------------------------------------
// Collapse rendering (formerly fold_render_tests / fold_toggle_tests / part of live)
// ------------------------------------------------------------------

#[test]
fn parallel_reads_collapse_to_one_line() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    for path in ["a.md", "b.md"] {
        chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            input: json!({"file_path": path}),
            standalone: false,
        });
        chat.drain_events();
    }
    let joined = visible(&mut chat, 120, 20);
    assert!(
        joined.contains("Reading 2 files"),
        "active summary: {joined}"
    );
    assert!(joined.contains("ctrl+o to expand"), "fold hint: {joined}");
    assert!(
        !joined.contains("a.md"),
        "paths hidden when collapsed: {joined}"
    );
}

/// Managing subagents used to produce one two-line block per call, all reading
/// `AgentControl(action="messages")` — the target was invisible (the k=v fallback takes the
/// alphabetically first key). One fold, counts that keep a stop apart, and a ⎿ row naming
/// the instance the latest call was aimed at.
#[test]
fn consecutive_agent_control_calls_fold_and_name_their_target() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    for input in [
        json!({"action": "messages", "agent": "scout"}),
        json!({"action": "messages", "agent": "reviewer"}),
        json!({"action": "stop", "agent": "scout"}),
    ] {
        chat.events.send(UiEvent::ToolStart {
            name: "AgentControl".into(),
        });
        chat.drain_events();
        chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "AgentControl".into(),
            input,
            standalone: false,
        });
        chat.drain_events();
    }
    assert_eq!(
        chat.conv.messages[0].groups.len(),
        1,
        "three calls, one group — not three blocks"
    );
    let joined = visible(&mut chat, 120, 20);
    assert!(
        joined.contains("Checking 2 subagents, stopping 1 subagent"),
        "the stop is not counted as a look: {joined}"
    );
    assert!(
        joined.contains("⎿  stop scout"),
        "the ⎿ row names the latest call and its target: {joined}"
    );
}

/// A tool the classifier did not know closed the open group, so a subagent check in the
/// middle of file work split one fold into three blocks.
#[test]
fn an_agent_control_call_no_longer_breaks_a_file_group() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    for (name, input) in [
        ("Read", json!({"file_path": "a.md"})),
        ("AgentControl", json!({"action": "list"})),
        ("Read", json!({"file_path": "b.md"})),
    ] {
        chat.events.send(UiEvent::ToolStart { name: name.into() });
        chat.drain_events();
        chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: name.into(),
            input,
            standalone: false,
        });
        chat.drain_events();
    }
    assert_eq!(chat.conv.messages[0].groups.len(), 1, "still one group");
    let joined = visible(&mut chat, 120, 20);
    assert!(
        joined.contains("Reading 2 files, checking 1 subagent"),
        "both kinds counted on one line: {joined}"
    );
}

/// A failure inside a fold used to be invisible: the summary counted the call as if it had
/// worked and only ctrl+o showed the error row. It matters most now that stopping a subagent
/// folds too — "stopped 1 subagent" must not stand for a stop that was refused.
#[test]
fn a_failure_inside_the_fold_is_named_on_the_summary_row() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    for input in [
        json!({"action": "stop", "agent": "ghost"}),
        json!({"action": "list"}),
    ] {
        chat.events.send(UiEvent::ToolStart {
            name: "AgentControl".into(),
        });
        chat.drain_events();
        chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "AgentControl".into(),
            input,
            standalone: false,
        });
        chat.drain_events();
    }
    chat.events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "AgentControl".into(),
            summary: "stop ghost".into(),
            output: "no subagent named ghost".into(),
            status: crate::query::ToolCallStatus::Error,
            duration_ms: 0,
            diff: None,
        }));
    chat.drain_events();
    let joined = visible(&mut chat, 120, 20);
    assert!(
        joined.contains("· 1 failed"),
        "the folded failure is named: {joined}"
    );
}

#[test]
fn group_done_uses_past_tense() {
    let mut chat = test_chat();
    start_group(&mut chat);
    finish_turn(&mut chat);
    let joined = visible(&mut chat, 120, 20);
    assert!(joined.contains("Read 2 files"), "past tense: {joined}");
}

#[test]
fn ctrl_o_expands_group_to_individual_tools() {
    let mut chat = test_chat();
    start_group(&mut chat);
    chat.events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: "Read a.md".into(),
            output: "l1\nl2\nl3".into(),
            status: crate::query::ToolCallStatus::Done,
            duration_ms: 0,
            diff: None,
        }));
    chat.events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: "Read b.md".into(),
            output: "x\ny".into(),
            status: crate::query::ToolCallStatus::Done,
            duration_ms: 0,
            diff: None,
        }));
    chat.drain_events();
    assert!(chat.expand_all_folds());
    let joined = visible(&mut chat, 120, 30);
    assert!(
        joined.contains("Read a.md"),
        "expanded first tool: {joined}"
    );
    assert!(
        joined.contains("Read b.md"),
        "expanded second tool: {joined}"
    );
    assert!(
        joined.contains("Read 3 lines"),
        "result summary row: {joined}"
    );
    assert!(
        !joined.contains("Reading 2 files"),
        "no collapse line: {joined}"
    );
}

#[test]
fn non_collapsible_tool_breaks_group() {
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    chat.events.send(UiEvent::ToolStart {
        name: "Read".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: json!({"file_path": "a.md"}),
        standalone: false,
    });
    chat.events.send(UiEvent::ToolStart {
        name: "WebSearch".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "WebSearch".into(),
        input: json!({"query": "rust"}),
        standalone: false,
    });
    chat.drain_events();
    let joined = visible(&mut chat, 120, 20);
    assert!(joined.contains("Read 1 file"), "group rendered: {joined}");
    assert!(
        joined.contains("WebSearch"),
        "websearch independent: {joined}"
    );
    assert!(
        !joined.contains("Reading"),
        "group closed by websearch: {joined}"
    );
}

#[test]
fn tool_after_thinking_placeholder_groups_without_panic() {
    // Regression: a tool right after the TurnStart placeholder thinking — group_of must stay in sync with activities.
    let mut chat = test_chat();
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    chat.apply_turn_start();
    chat.events.send(UiEvent::ToolStart {
        name: "Read".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: json!({"file_path": "a.md"}),
        standalone: false,
    });
    chat.drain_events();
    let joined = visible(&mut chat, 120, 30);
    assert!(joined.contains("Reading 1 file"), "group row: {joined}");
}

// ---- Interactions (CC feel): caret editing / history / multiline / double-press semantics / queueing ----

/// Chat with a dedicated home: history files are split per home, so tests never cross-contaminate.
pub(super) fn chat_with_history(tag: &str) -> Chat {
    let home = std::env::temp_dir().join(format!("bingo-chat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    test_chat_home(home)
}

thread_local! {
    static KEY_TICK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test key clock: every key advances 50ms — far above the paste-burst threshold, so
/// "rapid typing in tests" is never misjudged as a paste (same as real typing).
pub(super) fn key_time() -> std::time::Instant {
    let n = KEY_TICK.with(|c| {
        let v = c.get() + 1;
        c.set(v);
        v
    });
    std::time::Instant::now() + std::time::Duration::from_millis(50 * n)
}

pub(super) fn press(chat: &mut Chat, code: KeyCode) -> bool {
    chat.on_key_at(code, KeyModifiers::empty(), key_time())
}

pub(super) fn ctrl(chat: &mut Chat, c: char) -> bool {
    chat.on_key_at(KeyCode::Char(c), KeyModifiers::CONTROL, key_time())
}

pub(super) fn type_text(chat: &mut Chat, text: &str) {
    for c in text.chars() {
        press(chat, KeyCode::Char(c));
    }
}

pub(super) fn alt(chat: &mut Chat, c: char) -> bool {
    chat.on_key_at(KeyCode::Char(c), KeyModifiers::ALT, key_time())
}

#[test]
fn stream_retry_resets_only_current_attempt_and_replaces_progress_warning() {
    let home =
        std::env::temp_dir().join(format!("bingo-stream-retry-reset-{}", std::process::id()));
    let mut chat = test_chat_home(home.clone());
    chat.apply_event(UiEvent::TurnStart);
    chat.apply_event(UiEvent::TextDelta("committed".into()));
    chat.apply_event(UiEvent::RoundEnd);
    chat.apply_event(UiEvent::TextDelta("partial".into()));
    chat.apply_event(UiEvent::ToolStart {
        name: "Read".into(),
    });
    chat.apply_event(UiEvent::StreamRetry);
    chat.apply_event(UiEvent::Warning("Reconnecting... 2/10".into()));
    chat.apply_event(UiEvent::Warning("Reconnecting... 3/10".into()));

    let index = chat.conv.stream_msg.unwrap();
    assert_eq!(chat.conv.messages[index].text, "committed");
    assert_eq!(chat.conv.messages[index].activities.len(), 2);
    assert!(chat.conv.pending_tools.is_empty());
    assert_eq!(chat.visible_warning(), Some("Reconnecting... 3/10"));
    let _ = std::fs::remove_dir_all(home);
}
