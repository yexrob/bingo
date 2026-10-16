use super::*;
use bingo_services::mcp::{Auth, ServerRow, Status};
use serde_json::json;
fn fixture() -> (Vec<CatalogEntry>, Vec<ServerRow>) {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/panel.json")).unwrap();
    (
        serde_json::from_value(v["plugins"].clone()).unwrap(),
        serde_json::from_value(v["servers"].clone()).unwrap(),
    )
}
#[test]
fn groups_keep_the_opener_first_and_cursor_on_its_name() {
    let (plugins, mut servers) = fixture();
    let groups = groups::groups(component::PluginsOpen::Plugins, &plugins, &servers, false);
    assert_eq!(
        groups.iter().map(|g| g.name).collect::<Vec<_>>(),
        [
            "plugins · yours",
            "plugins · this project",
            "plugins · built in",
            "MCP servers"
        ]
    );
    assert_eq!(groups[1].rows[0].tag, "waits for your yes");
    let mut cursor = Cursor {
        selected: "m:linear".into(),
        ..Default::default()
    };
    servers.reverse();
    let groups = groups::groups(component::PluginsOpen::Mcp, &plugins, &servers, false);
    assert_eq!(groups[0].name, "MCP servers");
    assert_eq!(
        groups::selected(&groups, &mut cursor).unwrap().label,
        "linear"
    );
}
#[test]
fn mcp_opens_with_its_first_server_selected_before_builtins() {
    let (plugins, servers) = fixture();
    let mut cursor = Cursor::default();
    let props = component::PluginsProps {
        open: component::PluginsOpen::Mcp,
        session: SessionId::from_raw("s"),
        plugins,
        action: Action {
            name: "client".into(),
            args: json!({}),
        },
    };
    draw::tree(&props, &servers, &mut cursor, "mcp", 108, 20);
    assert_eq!(cursor.selected, format!("m:{}", servers[0].name));
}
#[test]
fn every_server_primary_action_is_a_command_and_reconnect_is_guarded() {
    let (_, mut servers) = fixture();
    let server = &mut servers[0];
    for (status, verb) in [
        (Status::Connected { tools: 3 }, "tools"),
        (Status::NeedsAuth { why: "401".into() }, "login"),
        (Status::Failed { why: "exit".into() }, "reconnect"),
        (Status::Disabled, "enable"),
    ] {
        server.status = status;
        assert_eq!(
            verbs::server(server, false, "enter"),
            verbs::Verb::Line(format!("/mcp {verb} github"))
        );
    }
    for status in [
        Status::Connecting,
        Status::SigningIn,
        Status::Disabled,
        Status::NeedsAuth { why: "401".into() },
    ] {
        server.status = status;
        assert!(matches!(
            verbs::server(server, false, "r"),
            verbs::Verb::Message(_)
        ));
    }
    server.auth = Some(Auth::SignedIn);
    assert_eq!(
        verbs::server(server, false, "o"),
        verbs::Verb::Line("/mcp logout github".into())
    );
}
#[test]
fn switches_use_the_wish_and_needed_plugins_are_refused() {
    let (plugins, _) = fixture();
    assert_eq!(
        verbs::plugin(&plugins[0], "space"),
        verbs::Verb::Line("/plugins disable my-picker".into())
    );
    assert_eq!(
        verbs::plugin(&plugins[6], "space"),
        verbs::Verb::Line("/plugins enable bingo.tools.bash".into())
    );
    assert!(matches!(
        verbs::plugin(&plugins[5], "space"),
        verbs::Verb::Message(_)
    ));
    assert_eq!(verbs::plugin(&plugins[3], "enter"), verbs::Verb::Trust);
    assert_eq!(
        verbs::plugin(&plugins[1], "r"),
        verbs::Verb::Line("/plugins reload".into())
    );
}
#[test]
fn full_requires_two_enters_and_moving_resets_confirmation() {
    let (plugins, _) = fixture();
    let p = &plugins[3];
    let mut choice = trust::Choice {
        selected: 2,
        confirm: false,
    };
    assert_eq!(trust::press(&mut choice, p, "enter"), None);
    assert!(choice.confirm);
    assert_eq!(
        trust::press(&mut choice, p, "enter"),
        Some(verbs::Verb::Line(
            "/plugins approve plugins:demo-lint full".into()
        ))
    );
    trust::press(&mut choice, p, "up");
    assert!(!choice.confirm);
    choice.selected = 3;
    assert!(matches!(
        trust::press(&mut choice, p, "enter"),
        Some(verbs::Verb::Message(_))
    ));
}
#[test]
fn cards_preserve_errors_and_name_trust_storage() {
    let (plugins, servers) = fixture();
    let error = cards::plugin_card(&plugins[1], &plugins, None)
        .into_iter()
        .find(|line| line.error)
        .unwrap();
    assert_eq!(
        error.text,
        format!(
            " {}",
            plugins[1].meta["state"]["why"]
                .as_str()
                .unwrap()
                .lines()
                .next()
                .unwrap()
        )
    );
    let card = cards::plugin_card(&plugins[3], &plugins, Some(&trust::Choice::default()));
    assert!(
        card.iter()
            .any(|line| line.text.contains("~/.bingo/settings.toml, never the repo"))
    );
    assert!(cards::server_card(&servers[0], false).iter().any(|line| {
        line.text
            .contains("always ask; an MCP tool is never trusted")
    }));
}
#[test]
fn handlers_retire_the_opening_and_return_only_command_lines() {
    let (plugins, servers) = fixture();
    let props = component::PluginsProps {
        open: component::PluginsOpen::Plugins,
        session: SessionId::from_raw("s"),
        plugins,
        action: Action {
            name: "surface:plugins".into(),
            args: json!({"handler":"a"}),
        },
    };
    let state = Shared::default();
    assert_eq!(
        press(&state, "a", "space", &props, &servers),
        json!({"run":"/plugins disable my-picker"})
    );
    assert_eq!(
        press(&state, "a", "esc", &props, &servers),
        json!({"close":true})
    );
    assert!(state.lock().unwrap().is_empty());
    state.lock().unwrap().insert(
        (props.session.clone(), "b".into()),
        Cursor {
            selected: "m:github".into(),
            ..Default::default()
        },
    );
    assert_eq!(
        press(&state, "b", "enter", &props, &servers),
        json!({"run":"/mcp tools github"})
    );
    assert!(state.lock().unwrap().is_empty());
}

#[test]
fn removing_the_selected_row_drops_its_unconfirmed_trust_choice() {
    let (mut plugins, servers) = fixture();
    let mut cursor = Cursor {
        selected: "p:plugins:demo-lint".into(),
        trust: Some(trust::Choice {
            selected: 2,
            confirm: true,
        }),
        ..Default::default()
    };
    plugins.retain(|p| p.id != "plugins:demo-lint");
    let rows = groups::groups(component::PluginsOpen::Plugins, &plugins, &servers, false);
    groups::selected(&rows, &mut cursor);
    assert!(cursor.trust.is_none());
}

#[test]
fn approval_keeps_the_project_rows_identity() {
    let (mut plugins, servers) = fixture();
    plugins[3].id = "demo-lint".into();
    plugins[3].meta["state"] = json!({"kind":"on"});
    let mut cursor = Cursor {
        selected: "p:plugins:demo-lint".into(),
        ..Default::default()
    };
    let rows = groups::groups(component::PluginsOpen::Plugins, &plugins, &servers, false);
    assert_eq!(
        groups::selected(&rows, &mut cursor).unwrap().label,
        "demo-lint"
    );
}

#[test]
fn opening_links_never_wrap_other_commands_or_subcommands() {
    for name in ["plugins", "mcp"] {
        let matcher = opening::Opening(name).matcher();
        assert!(matcher.matches(catalogue::COMMAND_RUN, &json!({"command":name,"args":""})));
        assert!(!matcher.matches(
            catalogue::COMMAND_RUN,
            &json!({"command":name,"args":"disable local"})
        ));
        assert!(!matcher.matches(
            catalogue::COMMAND_RUN,
            &json!({"command":"thinking","args":"maximum"})
        ));
    }
}

/// The footer is read off the same table the keys are: a key it names does
/// what it says, and a key it leaves out only explains itself.
#[test]
fn the_footer_names_exactly_the_keys_that_act() {
    let (plugins, mut servers) = fixture();
    let mut tables = plugins.iter().map(verbs::plugin_keys).collect::<Vec<_>>();
    for status in [
        Status::Connected { tools: 3 },
        Status::NeedsAuth { why: "401".into() },
        Status::Failed { why: "exit".into() },
        Status::Disabled,
        Status::Connecting,
        Status::Waiting { changed: false },
    ] {
        servers[0].status = status;
        tables.push(verbs::server_keys(&servers[0], false));
        tables.push(verbs::server_keys(&servers[0], true));
    }
    for table in tables {
        let footer = draw::footer(&table, "close");
        for binding in &table {
            let named = binding.hint.is_some();
            let quiet = matches!(binding.verb, verbs::Verb::Message(_) | verbs::Verb::None);
            assert_eq!(named, !quiet, "{binding:?} in {footer}");
        }
    }
}

#[test]
fn a_waiting_server_starts_nothing_until_a_second_enter() {
    let (_, mut servers) = fixture();
    servers[0].status = Status::Waiting { changed: false };
    let footer = draw::footer(&verbs::server_keys(&servers[0], false), "close");
    assert_eq!(footer, " ↑↓ pick · ⏎ review · esc close");
    for key in ["space", " ", "r"] {
        assert!(matches!(
            verbs::server(&servers[0], false, key),
            verbs::Verb::Message(_)
        ));
    }
    assert_eq!(
        verbs::server(&servers[0], false, "enter"),
        verbs::Verb::Review
    );
    assert_eq!(
        verbs::server(&servers[0], true, "enter"),
        verbs::Verb::Line("/mcp approve github".into())
    );
    assert!(matches!(
        verbs::server(&servers[0], true, "space"),
        verbs::Verb::Message(_)
    ));
}

#[test]
fn approving_a_server_shows_what_it_runs_and_moving_forgets_it() {
    let (plugins, mut servers) = fixture();
    servers[0].status = Status::Waiting { changed: false };
    let props = component::PluginsProps {
        open: component::PluginsOpen::Mcp,
        session: SessionId::from_raw("s"),
        plugins,
        action: Action {
            name: "surface:plugins".into(),
            args: json!({"handler":"a"}),
        },
    };
    let state = Shared::default();
    let press = |key| press(&state, "a", key, &props, &servers);
    assert!(press("space")["message"].is_string());
    assert_eq!(press("enter"), json!({}));
    let card = cards::server_card(&servers[0], true);
    assert!(card.iter().any(|fact| fact.text.contains("local-fixture")));
    let written = "./scripts/serve-mcp --listen 127.0.0.1:9 --then-run ./post-install.sh --quiet";
    let mut words = written.split(' ').map(String::from);
    let mut repo = servers[0].clone();
    repo.transport = bingo_services::mcp::Transport::Stdio {
        command: words.next().unwrap(),
        args: words.collect(),
        env: [("DEPLOY_TOKEN".into(), "s3cret".into())].into(),
        cwd: Some("tools".into()),
    }
    .reviewed();
    let card = cards::server_card(&repo, true);
    let said = card
        .iter()
        .map(|fact| fact.text.as_str())
        .collect::<Vec<_>>();
    assert!(said.iter().any(|text| text.trim() == written), "{said:?}");
    assert!(
        said.iter()
            .any(|text| text.contains("env DEPLOY_TOKEN (values hidden)")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|text| text.contains("in ./tools, from where bingo started")),
        "{said:?}"
    );
    assert!(!said.concat().contains("s3cret"), "{said:?}");
    assert!(
        cards::server_card(&repo, false)
            .iter()
            .all(|fact| !fact.text.contains("post-install.sh --quiet")),
        "a card that is not reviewing keeps the line short"
    );
    assert!(
        card.iter()
            .any(|fact| fact.error && fact.text.contains("approve github for this checkout"))
    );
    assert_eq!(press("down"), json!({}));
    assert_eq!(press("up"), json!({}));
    assert_eq!(press("enter"), json!({}), "moving away forgot the review");
    assert_eq!(press("enter"), json!({"run":"/mcp approve github"}));
    assert_eq!(press("enter"), json!({}), "a yes starts a new review");
    assert_eq!(
        press("esc"),
        json!({}),
        "esc ends the review, not the panel"
    );
    assert_eq!(press("esc"), json!({"close":true}));
}

/// Draws the panel the way the kernel would before a key reaches it, then
/// presses the key against the row that drawing showed.
fn press(
    state: &Shared,
    request: &str,
    key: &str,
    props: &component::PluginsProps,
    servers: &[ServerRow],
) -> serde_json::Value {
    let row = drawn_row(state, request, props, servers);
    pressed_on(state, request, key, Some(&row), props, servers)
}
fn drawn_row(
    state: &Shared,
    request: &str,
    props: &component::PluginsProps,
    servers: &[ServerRow],
) -> String {
    let mut guard = state.lock().unwrap();
    let cursor = guard
        .entry((props.session.clone(), request.into()))
        .or_default();
    let View::Box { layout, .. } = draw::tree(props, servers, cursor, request, 108, 30) else {
        panic!("panel")
    };
    layout.on_key.unwrap().args["row"].as_str().unwrap().into()
}
fn pressed_on(
    state: &Shared,
    request: &str,
    key: &str,
    row: Option<&str>,
    props: &component::PluginsProps,
    servers: &[ServerRow],
) -> serde_json::Value {
    let at = keys::Press {
        session: &props.session,
        request,
        key,
        row,
        vouched: true,
    };
    keys::Handler(state.clone())
        .press(&at, props, servers)
        .unwrap()
}

/// A key answers the drawing it was pressed on: when the cursor has moved on
/// since, or the row that drawing showed is gone, it acts on nothing.
#[test]
fn a_key_from_a_stale_drawing_acts_on_nothing() {
    let (plugins, mut servers) = fixture();
    let props = component::PluginsProps {
        open: component::PluginsOpen::Mcp,
        session: SessionId::from_raw("s"),
        plugins,
        action: Action {
            name: "surface:plugins".into(),
            args: json!({"handler":"a"}),
        },
    };
    let state = Shared::default();
    let seen = drawn_row(&state, "a", &props, &servers);
    assert_eq!(seen, "m:github");
    assert_eq!(
        pressed_on(&state, "a", "down", Some(&seen), &props, &servers),
        json!({})
    );
    let stale = pressed_on(&state, "a", "enter", Some(&seen), &props, &servers);
    assert!(
        stale.get("run").is_none() && stale["message"].is_string(),
        "{stale}"
    );
    assert!(
        pressed_on(&state, "a", "space", None, &props, &servers)
            .get("run")
            .is_none()
    );
    let linear = drawn_row(&state, "a", &props, &servers);
    assert_eq!(linear, "m:linear");
    servers.retain(|s| s.name != "linear");
    let gone = pressed_on(&state, "a", "space", Some(&linear), &props, &servers);
    assert!(gone.get("run").is_none(), "{gone}");
    assert_eq!(
        state.lock().unwrap()[&(props.session.clone(), "a".to_string())].selected,
        "m:github",
        "a vanished row resets the cursor to the first"
    );
    assert_eq!(
        press(&state, "a", "space", &props, &servers),
        json!({"run":"/mcp disable github"})
    );
}

/// Every text line a drawing holds, with its colour.
fn lines(view: &View) -> Vec<(String, Option<String>)> {
    fn walk(value: &serde_json::Value, out: &mut Vec<(String, Option<String>)>) {
        match value {
            serde_json::Value::Object(map) => {
                let children = map.get("children").and_then(|c| c.as_array());
                let strings = children
                    .map(|c| c.iter().filter_map(|c| c.as_str()).collect::<String>())
                    .filter(|text| !text.is_empty());
                if let Some(text) = strings {
                    let color = map.get("color").and_then(|c| c.as_str()).map(String::from);
                    out.push((text, color));
                }
                map.values().for_each(|v| walk(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    walk(&serde_json::to_value(view).unwrap(), &mut out);
    out
}
fn props_of(plugins: Vec<CatalogEntry>) -> component::PluginsProps {
    component::PluginsProps {
        open: component::PluginsOpen::Plugins,
        session: SessionId::from_raw("s"),
        plugins,
        action: Action {
            name: "surface:plugins".into(),
            args: json!({"handler":"a"}),
        },
    }
}
fn drawn_at(
    props: &component::PluginsProps,
    servers: &[ServerRow],
    row: &str,
    width: u16,
    height: u16,
) -> Vec<(String, Option<String>)> {
    let mut cursor = Cursor {
        selected: row.into(),
        ..Default::default()
    };
    let view = draw::tree(props, servers, &mut cursor, "a", width, height);
    bingo_sdk::testing::assert_separate_hints(&view);
    lines(&view)
}

#[test]
fn error_lines_stay_inside_the_card_and_are_cut_at_its_edge() {
    let (mut plugins, servers) = fixture();
    plugins[1].meta["state"]["why"] = json!(format!("{}\n  second line", "x".repeat(200)));
    for (width, height) in [(80, 24), (108, 30)] {
        let drawn = drawn_at(
            &props_of(plugins.clone()),
            &servers,
            "p:weather",
            width,
            height,
        );
        let red = drawn
            .iter()
            .filter(|(text, color)| color.as_deref() == Some("red") && !text.contains('✗'))
            .map(|(text, _)| text)
            .collect::<Vec<_>>();
        assert_eq!(red.len(), 2, "{red:?}");
        assert!(red.iter().all(|text| text.starts_with(' ')));
        assert!(red[0].ends_with('…'));
        let card = if width < 100 {
            width
        } else {
            width - width * 56 / 108 - 1
        };
        assert_eq!(red[0].chars().count(), usize::from(card));
        assert_eq!(red[1], "   second line");
    }
}

#[test]
fn a_long_list_says_how_many_rows_it_hides() {
    let (plugins, servers) = fixture();
    let props = props_of(plugins);
    let drawn = drawn_at(&props, &servers, "p:my-picker", 80, 24);
    assert!(
        drawn
            .iter()
            .any(|(text, _)| text.trim_start().starts_with('↓') && text.ends_with("more")),
        "{drawn:?}"
    );
    let drawn = drawn_at(&props, &servers, "m:figma", 80, 24);
    assert!(
        drawn
            .iter()
            .any(|(text, _)| text.trim_start().starts_with('↑') && text.ends_with("above")),
        "{drawn:?}"
    );
}

#[test]
fn a_long_origin_keeps_its_start_and_end() {
    let path = "this project · /Users/someone/work/a-very-long-checkout-name/.mcp.json";
    let cut = cards::middle(path, 38);
    assert_eq!(cut.chars().count(), 38);
    assert!(cut.starts_with("this project · /"));
    assert!(cut.ends_with("/.mcp.json"));
    assert!(cut.contains('…'));
    assert_eq!(cards::middle("short", 38), "short");
    let (_, mut servers) = fixture();
    servers[0].origin = Some(path.into());
    let card = cards::server_card(&servers[0], false);
    assert!(card.iter().any(|fact| fact.text.ends_with(&cut)));
}

/// A long command line would wrap under the card's keys; it keeps the
/// program and its end, as a long origin does.
#[test]
fn a_long_command_keeps_its_program_and_its_end() {
    let (_, mut servers) = fixture();
    servers[0].transport = bingo_services::mcp::Transport::Stdio {
        command: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "touch /a/very/long/path/that/would/wrap/under/the/card/MARKER; sleep 30".into(),
        ],
        env: Default::default(),
        cwd: None,
    };
    let card = cards::server_card(&servers[0], false);
    let runs = card
        .iter()
        .find(|fact| fact.text.starts_with(" runs "))
        .expect("a runs row");
    let value = runs.text.trim_start_matches(" runs").trim_start();
    assert_eq!(value.chars().count(), 38, "{value}");
    assert!(value.starts_with("/bin/sh -c") && value.ends_with("sleep 30"));
}

#[test]
fn a_failed_server_card_says_its_last_stderr_line() {
    let (_, servers) = fixture();
    let postgres = servers.iter().find(|s| s.name == "postgres").unwrap();
    let card = cards::server_card(postgres, false);
    assert!(card.iter().any(|fact| fact.error
        && fact.text == " its stderr: psql: could not connect to server: Connection refused"));
}

#[test]
fn the_built_ins_card_counts_the_ones_that_stay_on() {
    let (plugins, _) = fixture();
    let card = cards::builtins_card(&plugins, false);
    let text = card
        .iter()
        .map(|fact| fact.text.as_str())
        .collect::<Vec<_>>();
    assert!(text.contains(&" 1 are needed and stay on"), "{text:?}");
    assert!(text.contains(&" show all 3"), "{text:?}");
    let folded = cards::builtins_card(&plugins, true);
    assert!(folded.iter().any(|fact| fact.text == " fold them"));
}

#[test]
fn an_http_server_that_never_asked_names_no_oauth() {
    let (_, mut servers) = fixture();
    let server = &mut servers[0];
    server.transport = bingo_services::mcp::Transport::Http {
        url: "https://mcp.example.com/mcp".into(),
        headers: Default::default(),
    };
    server.auth = Some(Auth::SignedOut);
    server.status = Status::Connected { tools: 2 };
    let oauth = |s: &ServerRow| {
        cards::server_card(s, false)
            .iter()
            .any(|f| f.text.contains("OAuth"))
    };
    assert!(!oauth(server));
    server.status = Status::NeedsAuth { why: "401".into() };
    assert!(oauth(server));
}

#[test]
fn a_pending_switch_says_the_states_own_reason() {
    let (mut plugins, servers) = fixture();
    plugins[6].meta["state"] = json!({"kind":"pending","now":true,"at":"afterTurn"});
    let groups = groups::groups(component::PluginsOpen::Plugins, &plugins, &servers, true);
    let row = groups
        .iter()
        .flat_map(|g| &g.rows)
        .find(|r| r.label == "bingo.tools.bash")
        .unwrap();
    assert_eq!(row.tag, "off after the running turn");
    let card = cards::plugin_card(&plugins[6], &plugins, None);
    assert!(
        card.iter()
            .any(|f| f.text.ends_with("● on → off after the running turn"))
    );
}

#[test]
fn a_state_this_build_cannot_read_is_unknown() {
    let (mut plugins, servers) = fixture();
    plugins[0].meta["state"] = json!({"kind":"hibernating"});
    let groups = groups::groups(component::PluginsOpen::Plugins, &plugins, &servers, false);
    let row = &groups[0].rows[0];
    assert_eq!((row.glyph, row.tag.as_str()), ("?", "unknown"));
    let card = cards::plugin_card(&plugins[0], &plugins, None);
    assert!(card.iter().any(|f| f.text.ends_with("? unknown")));
    assert!(!card.iter().any(|f| f.text.contains("loading")));
    assert!(matches!(
        verbs::plugin(&plugins[0], "space"),
        verbs::Verb::None
    ));
}

#[test]
fn only_a_built_in_that_draws_a_site_falls_back_to_the_plain_drawing() {
    let (mut plugins, _) = fixture();
    let plain = |p: &CatalogEntry, all: &[CatalogEntry]| {
        cards::plugin_card(p, all, None)
            .iter()
            .any(|f| f.text.contains("the plain drawing is used"))
    };
    assert!(!plain(&plugins[4], &plugins), "draws nothing yet");
    plugins[4].meta["links"] = json!([{"order":5,"matcher":LinkMatcher {
        prefilter: json!({"component":"Palette"}),
        ..LinkMatcher::at(catalogue::UI_RENDER)
    }}]);
    assert!(plain(&plugins[4], &plugins));
    plugins[0].meta["links"] = plugins[4].meta["links"].clone();
    assert!(
        !plain(&plugins[0], &plugins),
        "an outside mod is not built in"
    );
}

#[test]
fn one_server_is_counted_as_one() {
    assert_eq!(crate::draw::counted(1, "MCP server"), "1 MCP server");
    assert_eq!(crate::draw::counted(2, "plugin"), "2 plugins");
    assert_eq!(crate::draw::counted(0, "plugin"), "0 plugins");
}
