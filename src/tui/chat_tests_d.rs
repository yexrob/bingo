//! Chat state-machine tests, part four: the composer's completion surfaces
//! (D85, D103) — the `@`/`#` typeahead and slash **argument** completion.
//!
//! `chat_tests_a` / `b` / `c` split by size alone (the 4000-line file cap);
//! this file continues them.

use super::chat_tail::EscLayer;
use super::tests_a::*;
use super::*;
use crate::tui::complete::{ArgCandidate, MentionKind, arg_context};

/// A project directory with a known shape, outside any git repository, so the
/// fallback walker is what answers. Created here and removed here.
fn project_dir(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("bingo-d85-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(root.join("src/tui"));
    let _ = std::fs::write(root.join("README.md"), "x");
    let _ = std::fs::write(root.join("src/main.rs"), "x");
    let _ = std::fs::write(root.join("src/tui/chat.rs"), "x");
    root
}

/// A chat rooted in that project. `test_chat_home` points cwd at home, which
/// is exactly what the mention source reads.
fn project_chat(tag: &str) -> (Chat, std::path::PathBuf) {
    let root = project_dir(tag);
    let chat = test_chat_home(root.clone());
    (chat, root)
}

/// A chat whose client carries a declared model catalog (D73). The catalog
/// only exists on a `Client` built from settings.
fn chat_with_settings(tag: &str, json: &str) -> Chat {
    let home = std::env::temp_dir().join(format!("bingo-d85-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::create_dir_all(&home);
    let mut chat = test_chat_home(home.clone());
    let settings: crate::settings::Settings =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("settings: {e}"));
    let session = crate::tui::test_util::session_mut(&mut chat);
    session.client = crate::api::client::Client::from_settings_at(&settings, &home)
        .unwrap_or_else(|e| panic!("client: {e}"));
    session.settings = settings;
    chat
}

/// The values the dropdown is offering, in order.
fn offered(chat: &Chat) -> Vec<String> {
    chat.slash_suggestions
        .iter()
        .map(|s| s.name.clone())
        .collect()
}

/// The mention rows the user would see.
fn mention_values(chat: &Chat) -> Vec<String> {
    chat.mention
        .as_ref()
        .map(|state| {
            state
                .items
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// `@` mention dropdown
// ---------------------------------------------------------------------------

/// `@` at a word boundary opens the dropdown over the project's files, and
/// what follows it narrows the list.
#[test]
fn mention_opens_at_a_word_boundary_and_narrows() {
    let (mut chat, root) = project_chat("open");

    chat.set_input("@");
    let all = mention_values(&chat);
    assert!(chat.mention.is_some(), "a bare @ opens the dropdown");
    assert!(
        all.contains(&"src/tui/chat.rs".to_string()) && all.contains(&"README.md".to_string()),
        "every project file is offered: {all:?}"
    );

    chat.set_input("@chat");
    let narrowed = mention_values(&chat);
    assert_eq!(
        narrowed,
        vec!["src/tui/chat.rs".to_string()],
        "the query filters the same snapshot"
    );

    // Mid-sentence, after a space, is still a word boundary.
    chat.set_input("please read @main");
    assert_eq!(mention_values(&chat), vec!["src/main.rs".to_string()]);

    let _ = std::fs::remove_dir_all(&root);
}

/// An `@` inside a word is an address, not a mention.
#[test]
fn mention_does_not_open_inside_a_token() {
    let (mut chat, root) = project_chat("email");

    chat.set_input("mail me at user@example.com");
    assert!(chat.mention.is_none(), "an email address opens nothing");
    chat.set_input("a@b");
    assert!(chat.mention.is_none());

    // The same characters after a space do open it.
    chat.set_input("mail me at @");
    assert!(chat.mention.is_some());

    let _ = std::fs::remove_dir_all(&root);
}

/// Selecting inserts the relative path plus one space, and closes.
#[test]
fn mention_selection_inserts_a_relative_path_and_closes() {
    let (mut chat, root) = project_chat("insert");

    chat.set_input("look at @chat");
    assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE), "tab inserts");
    assert_eq!(chat.input, "look at src/tui/chat.rs ");
    assert_eq!(chat.cursor, chat.input.len(), "the caret follows the text");
    assert!(chat.mention.is_none(), "the dropdown closed");

    // Enter is the other accept key, and it inserts rather than submitting.
    chat.set_input("@main");
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(chat.input, "src/main.rs ");
    assert!(chat.conv.messages.is_empty(), "nothing was sent");

    let _ = std::fs::remove_dir_all(&root);
}

/// Agents share the dropdown with files and keep their `@` — that token is what
/// the direct send reads.
///
/// Rewritten for D103, which gave the sigil two readings by position. **At the
/// start of a line** `@name ` bypasses the model, so the list is every instance
/// the send can reach, and each row says what Enter will do. **Mid-line** it is
/// D85's live reference, unchanged: running instances only, with no note.
///
/// *Stopped instances are still listed*, which is what D103 decided and what
/// D105 found to be half true: the composer offers them and the domain then
/// refuses (`AgentHandle::deliver`). The listing is left exactly as it is —
/// the gap is in the domain, and it is named where it lives (D105's record).
#[test]
fn mention_lists_agents_by_what_the_position_can_reach() {
    let (mut chat, root) = project_chat("agents");
    chat.session
        .agents
        .insert(
            "scout",
            crate::agents::AgentKind::Hire,
            None,
            "inspect the code".into(),
            chat.session.clone(),
        )
        .now();

    let agent_row = |chat: &Chat| {
        chat.mention
            .as_ref()
            .and_then(|state| {
                state
                    .items
                    .iter()
                    .find(|item| item.kind == MentionKind::Agent)
                    .cloned()
            })
            .unwrap_or_else(|| panic!("the agent is offered"))
    };

    chat.set_input("@scou");
    let leading = agent_row(&chat);
    assert_eq!(leading.value, "scout");
    assert_eq!(leading.insertion(), "@scout", "an agent keeps its @");
    assert_eq!(
        leading.note, "send message · running",
        "at the start of a line the row says the send is what Enter does"
    );

    chat.set_input("ask @scou");
    assert_eq!(
        agent_row(&chat).note,
        "",
        "mid-line it is a reference, and a reference does nothing on its own"
    );

    // A stopped agent is still offered by the typeahead (D103's ruling) —
    // but it is not a live reference.
    let _ = chat.session.agents.stop("scout").now();
    chat.set_input("x");
    chat.set_input("@scou");
    assert_eq!(agent_row(&chat).note, "send message · stopped");
    chat.set_input("x");
    chat.set_input("ask @scou");
    assert!(
        mention_values(&chat).iter().all(|v| v != "scout"),
        "only running agents are offered mid-line"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// D103: `#` at the start of a line offers the rooms, and says what posting to
/// one will do — including that speaking in a room you are not in joins you,
/// which is the only thing about the grammar a user could not guess.
#[test]
fn a_leading_hash_offers_the_rooms_and_what_a_post_will_do() {
    let (mut chat, root) = project_chat("rooms");
    chat.session
        .channels
        .create(
            "build",
            vec![crate::channels::USER_NAME.to_string()],
            crate::channels::ChannelMode::Free,
        )
        .now()
        .unwrap_or_else(|e| panic!("room: {e}"));
    chat.session
        .channels
        .create(
            "parser",
            vec!["scout".to_string()],
            crate::channels::ChannelMode::Free,
        )
        .now()
        .unwrap_or_else(|e| panic!("room: {e}"));

    chat.set_input("#");
    let items = chat
        .mention
        .as_ref()
        .map(|state| state.items.clone())
        .unwrap_or_default();
    assert!(
        items.iter().all(|item| item.kind == MentionKind::Room),
        "the room sigil has exactly one meaning: {items:?}"
    );
    let note = |name: &str| {
        items
            .iter()
            .find(|item| item.value == name)
            .unwrap_or_else(|| panic!("{name} is offered: {items:?}"))
            .clone()
    };
    assert_eq!(note("build").insertion(), "#build", "a room keeps its #");
    assert_eq!(note("build").note, "post to room");
    assert_eq!(
        note("parser").note,
        "post to room · joins you",
        "a room you are not in says that speaking is joining"
    );

    // Mid-line a hash is a hash.
    chat.set_input("see #42");
    assert!(chat.mention.is_none(), "no dropdown over an issue number");

    let _ = std::fs::remove_dir_all(&root);
}

/// The dropdown is a layer: Esc peels it and leaves the typed text alone.
#[test]
fn mention_esc_closes_the_layer_and_keeps_the_text() {
    let (mut chat, root) = project_chat("esc");

    chat.set_input("look at @chat");
    assert_eq!(chat.esc_layer(), Some(EscLayer::MentionDropdown));

    let t0 = std::time::Instant::now();
    assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
    assert!(chat.mention.is_none(), "the dropdown closed");
    assert_eq!(chat.input, "look at @chat", "the draft survived");
    assert_eq!(
        chat.esc_layer(),
        Some(EscLayer::ClearInput),
        "one press peeled exactly one layer"
    );

    // The dismissal sticks: typing on does not resurrect what Esc closed.
    assert!(chat.on_key_at(KeyCode::Char('x'), KeyModifiers::empty(), t0));
    assert!(chat.mention.is_none(), "esc stays honoured");

    let _ = std::fs::remove_dir_all(&root);
}

/// A permission dialog owns the keyboard: nothing typed behind it may open a
/// surface that competes for Enter (D80/D81).
#[test]
fn permission_dialog_keeps_the_mention_dropdown_closed() {
    let (mut chat, root) = project_chat("ask");
    chat.stub_ask(PermissionRequest::new(
        "Bash",
        "Allow running Bash?",
        vec!["Yes".into(), "No".into()],
    ));

    chat.set_input("@chat");
    assert!(chat.mention.is_none(), "the dialog wins");
    assert_eq!(
        chat.esc_layer(),
        Some(EscLayer::AskDialog),
        "the dialog is still the top layer"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The rendered dropdown labels its sections and states its keys.
#[test]
fn mention_rows_label_sections_and_carry_a_footer() {
    let (mut chat, root) = project_chat("render");
    chat.session
        .agents
        .insert(
            "scout",
            crate::agents::AgentKind::Hire,
            None,
            "inspect the code".into(),
            chat.session.clone(),
        )
        .now();

    chat.set_input("@");
    let state = chat.mention.as_ref().unwrap_or_else(|| panic!("open"));
    let text = crate::tui::complete::mention_rows(state, &chat.theme, 80)
        .iter()
        .map(|row| row.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Files"), "{text}");
    assert!(text.contains("Agents"), "{text}");
    assert!(text.contains("@scout"), "{text}");
    assert!(text.contains("tab/enter inserts"), "{text}");
    assert!(text.contains('❯'), "the selection is marked: {text}");

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Slash argument completion
// ---------------------------------------------------------------------------

/// `/model <partial>` offers ids read from the declared catalog itself — add a
/// model to settings and it appears, with no list to keep in sync.
#[test]
fn model_arguments_come_from_the_declared_catalog() {
    let mut chat = chat_with_settings(
        "catalog",
        r#"{"apiKey": "sk", "models": [
            "deepseek-chat",
            {"id": "deepseek-reasoner", "display": "DeepSeek R1"},
            "claude-opus-5"
        ]}"#,
    );

    chat.set_input("/model ");
    assert_eq!(
        offered(&chat),
        vec!["deepseek-chat", "deepseek-reasoner", "claude-opus-5"],
        "an empty argument lists the catalog in its own order"
    );
    assert!(
        chat.slash_arg_start.is_some(),
        "this is the argument phase, not the name phase"
    );

    chat.set_input("/model dee");
    assert_eq!(offered(&chat), vec!["deepseek-chat", "deepseek-reasoner"]);
    assert_eq!(
        chat.slash_suggestions[1].description, "DeepSeek R1",
        "the catalog's display name rides along"
    );

    chat.set_input("/model reason");
    assert_eq!(
        offered(&chat),
        vec!["deepseek-reasoner"],
        "fuzzy, not prefix"
    );
}

/// `/think` reads the same level table its handler validates against.
#[test]
fn think_arguments_offer_the_level_table() {
    let mut chat = test_chat();
    chat.set_input("/think ");
    assert_eq!(
        offered(&chat),
        crate::tui::chat::think_levels()
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect::<Vec<_>>()
    );
    // Fuzzy, so `xhigh` matches too — but the boundary match ranks first.
    chat.set_input("/think hi");
    assert_eq!(offered(&chat), vec!["high", "xhigh"]);
}

/// `/theme` likewise.
#[test]
fn theme_arguments_offer_the_theme_table() {
    let mut chat = test_chat();
    chat.set_input("/theme ");
    assert_eq!(offered(&chat), vec!["dark", "light", "auto"]);
    chat.set_input("/theme li");
    assert_eq!(offered(&chat), vec!["light"]);
}

/// The typeahead `/join`'s own usage line has been promising since D89. The
/// argument dropdown covered five commands out of twenty-four before the one
/// table said where each argument's values come from (D146).
#[test]
fn room_arguments_offer_the_rooms_that_exist() {
    let mut chat = test_chat();
    let _ = chat
        .session
        .channels
        .create(
            "build",
            vec!["main".to_string()],
            crate::channels::ChannelMode::Free,
        )
        .now();
    chat.set_input("/join ");
    assert_eq!(offered(&chat), vec!["build"]);
    chat.set_input("/leave bu");
    assert_eq!(offered(&chat), vec!["build"]);
}

/// A sub-command decides which argument comes next, so `/mcp enable` offers
/// servers where `/mcp` offers operations.
#[test]
fn mcp_arguments_walk_from_the_operation_to_the_server() {
    let mut chat = test_chat();
    chat.set_input("/mcp ");
    assert_eq!(offered(&chat), vec!["enable", "disable", "reconnect"]);
    chat.set_input("/mcp enable ");
    assert!(
        offered(&chat).is_empty(),
        "the test session configures no MCP servers, and no dropdown opens over nothing"
    );
}

/// `/provider` is two-shaped: the first token may be a subcommand or a
/// provider name, and after `login` only provider names remain.
#[test]
fn provider_arguments_complete_the_subcommand_then_the_provider() {
    let mut chat = test_chat();

    chat.set_input("/provider ");
    let first = offered(&chat);
    assert_eq!(
        &first[..2],
        &["login".to_string(), "logout".to_string()],
        "the subcommands lead: {first:?}"
    );
    assert!(
        first.contains(&"default".to_string()),
        "a bare name switches provider, so names are offered too: {first:?}"
    );

    chat.set_input("/provider log");
    assert_eq!(offered(&chat), vec!["login", "logout"]);

    chat.set_input("/provider login ");
    let names = offered(&chat);
    assert!(
        names.contains(&"codex".to_string()),
        "presets are login targets: {names:?}"
    );
    assert!(
        !names.contains(&"login".to_string()),
        "the subcommand is done, not offered again: {names:?}"
    );
    assert!(
        !names.contains(&"default".to_string()),
        "login rejects `default`, so it is never offered: {names:?}"
    );

    // A completed pair offers nothing more.
    chat.set_input("/provider login codex ");
    assert!(offered(&chat).is_empty(), "the argument list is exhausted");
}

/// `/resume` lists the sessions its own handler searches.
#[test]
fn resume_arguments_list_sessions() {
    let home = std::env::temp_dir().join(format!("bingo-d85-{}-resume", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut chat = test_chat_home(home.clone());
    let dir = crate::transcript::transcripts_dir(&home);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("proj-20260814-120000-alpha.jsonl"), "");
    let _ = std::fs::write(dir.join("proj-20260814-130000-beta.jsonl"), "");

    chat.set_input("/resume ");
    let names = offered(&chat);
    assert!(
        names.iter().any(|n| n.ends_with("alpha")) && names.iter().any(|n| n.ends_with("beta")),
        "every stored session is offered: {names:?}"
    );

    chat.set_input("/resume alph");
    assert_eq!(
        offered(&chat)
            .iter()
            .filter(|n| n.ends_with("beta"))
            .count(),
        0,
        "the query filters what /resume itself would search"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Free-form arguments open nothing — there is no domain to enumerate.
#[test]
fn free_form_arguments_offer_nothing() {
    let mut chat = test_chat();
    for line in [
        "/cd ",
        "/rename my session",
        "/team message hello",
        "/help ",
    ] {
        chat.set_input(line);
        assert!(
            chat.slash_suggestions.is_empty() && chat.slash_arg_start.is_none(),
            "no dropdown for {line:?}: {:?}",
            offered(&chat)
        );
        assert!(
            !chat.slash_no_match,
            "and no `no matching commands` hint either, for {line:?}"
        );
    }
}

/// Tab completes the argument in place, leaving the command and the earlier
/// arguments alone; nothing is submitted.
#[test]
fn tab_completes_an_argument_without_submitting() {
    let mut chat = chat_with_settings(
        "apply",
        r#"{"apiKey": "sk", "models": ["deepseek-chat", "claude-opus-5"]}"#,
    );

    chat.set_input("/model claude");
    assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(chat.input, "/model claude-opus-5 ");
    assert_eq!(chat.cursor, chat.input.len());
    assert!(chat.conv.messages.is_empty(), "tab does not send");

    // The two-token shape splices at the partial, not at the line start.
    chat.set_input("/provider login cod");
    assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(chat.input, "/provider login codex ");
}

/// Enter with an argument dropdown open runs the line the user typed. The
/// name-phase shortcut (Enter completes *and* executes) must not reach here:
/// it would dispatch `/deepseek-chat` as if it were a command.
#[test]
fn enter_in_the_argument_phase_runs_the_typed_command() {
    let mut chat = chat_with_settings(
        "enter",
        r#"{"apiKey": "sk", "models": ["deepseek-chat", "claude-opus-5"]}"#,
    );
    chat.set_input("/think high");
    assert!(!chat.slash_suggestions.is_empty(), "the dropdown is open");
    chat.submit();
    assert_eq!(
        chat.thinking().as_deref(),
        Some("high"),
        "the typed command ran"
    );
    assert!(
        all_slash_text(&chat).to_lowercase().contains("think"),
        "and it reported: {}",
        all_slash_text(&chat)
    );
}

/// Esc in the argument phase closes the dropdown but keeps the half-typed
/// command — unlike the name phase, where the bare `/` query goes with it.
#[test]
fn esc_in_the_argument_phase_keeps_the_command() {
    let mut chat = test_chat();
    chat.set_input("/think hi");
    assert_eq!(chat.esc_layer(), Some(EscLayer::SlashDropdown));
    let t0 = std::time::Instant::now();
    assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
    assert_eq!(chat.input, "/think hi", "the command survived");
    assert!(chat.slash_suggestions.is_empty());

    // The name phase still clears, as it always did.
    chat.set_input("/thi");
    assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
    assert_eq!(chat.input, "");
}

/// The argument phase renders values, not commands: the `/` in front of each
/// row is the one thing that goes away.
#[test]
fn argument_rows_drop_the_slash_prefix() {
    let mut chat = test_chat();
    chat.set_input("/think ");
    let rows = crate::tui::chrome::chrome(&chat, 100, false);
    let text = crate::tui::el::render(rows)
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("medium"), "the levels are listed: {text}");
    assert!(
        !text.contains("/medium"),
        "an argument is not a command: {text}"
    );
}

/// The name phase is untouched: `/mo` still lists commands, with their `/`.
#[test]
fn command_name_completion_is_unregressed() {
    let mut chat = test_chat();
    chat.set_input("/mo");
    assert!(
        chat.slash_arg_start.is_none(),
        "a name query is not an argument query"
    );
    assert!(offered(&chat).contains(&"model".to_string()));
    assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(chat.input, "/model ");
}

/// The registry is keyed on the command *and* on how many arguments are
/// already in: a domain runs out.
#[test]
fn the_argument_registry_is_arity_aware() {
    let chat = test_chat();
    let one = arg_context("/theme ").unwrap_or_else(|| panic!("argument phase"));
    assert!(chat.arg_candidates(&one).is_some());
    let two = arg_context("/theme dark ").unwrap_or_else(|| panic!("argument phase"));
    assert!(
        chat.arg_candidates(&two).is_none(),
        "/theme takes exactly one argument"
    );
    assert_eq!(
        ArgCandidate::new("dark", "dark theme").value,
        "dark",
        "candidates carry a value and a description"
    );
}
