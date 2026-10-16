use crate::{Shared, rows::Target};
use bingo_sdk::{component::PaletteProps, *};
use serde_json::{Value, json};
pub struct Open(pub &'static str);
#[async_trait::async_trait]
impl Command for Open {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            opens: false,
            name: self.0.into(),
            aliases: vec![],
            hint: if self.0 == "help" {
                "keys, in this list"
            } else {
                "reopen a session"
            }
            .into(),
            args: ArgSpec::None,
            instant: true,
            family: "terminal".into(),
            hidden: false,
            keys: if self.0 == "resume" {
                vec!["ctrl+r".into()]
            } else {
                vec![]
            },
        }
    }
    async fn run(&self, args: &str, _: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let value = if self.0 == "resume" && !args.trim().is_empty() {
            json!({"resume":args.trim()})
        } else {
            json!({"open":if self.0=="help"{"keys"}else{"sessions"}})
        };
        Ok(CommandOutcome::Surface {
            fallback: Some(format!("/{} is terminal only.", self.0)),
            action: Action {
                name: "Palette".into(),
                args: value,
            },
        })
    }
}
pub struct Handler(pub Shared);
#[async_trait::async_trait]
impl Command for Handler {
    fn spec(&self) -> CommandSpec {
        CommandSpec::ui("palette")
    }
    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let args: Value = serde_json::from_str(args).map_err(crate::error)?;
        let request = args["handler"].as_str().unwrap_or_default();
        let key = args["value"]["key"].as_str().unwrap_or_default();
        let Some(props) = self.drawn(&cx.session, request, &args["query"]) else {
            return Ok(CommandOutcome::Handled { handled: false });
        };
        if args["event"] != "press" {
            return Ok(CommandOutcome::Handled { handled: false });
        }
        let outcome = self.press(request, key, &props)?;
        cx.host
            .invalidate(&cx.session, vec![request.into()])
            .await?;
        Ok(outcome)
    }
}
impl Handler {
    /// The props the site was last drawn from, while that drawing is of the
    /// query the press was made at.
    fn drawn(
        &self,
        session: &SessionId,
        request: &str,
        query: &Value,
    ) -> Option<std::sync::Arc<PaletteProps>> {
        let shared = self.0.lock().ok()?;
        let drawn = shared
            .cursors
            .get(&(session.clone(), request.into()))?
            .drawn
            .clone()?;
        (query.as_str() == Some(drawn.query.as_str())).then_some(drawn)
    }
    fn press(
        &self,
        request: &str,
        key: &str,
        props: &PaletteProps,
    ) -> Result<CommandOutcome, KernelError> {
        let mut shared = self.0.lock().map_err(crate::error)?;
        let (cursor, groups) = shared.walk(&props.session, request, props);
        let rows = groups
            .iter()
            .flat_map(|group| &group.rows)
            .collect::<Vec<_>>();
        let target = rows.get(cursor.selected).map(|row| row.target.clone());
        let (handled, operation) = operation(cursor, &rows, target, key, props);
        if !handled {
            return Ok(CommandOutcome::Handled { handled: false });
        }
        let Some(operation) = operation else {
            return Ok(CommandOutcome::Handled { handled: true });
        };
        if let Some(line) = operation["run"].as_str() {
            shared.uses.count(line).map_err(crate::error)?;
        }
        if operation.get("close").is_some()
            || operation.get("run").is_some()
            || operation.get("resume").is_some()
            || operation.get("view").is_some()
            || operation.get("key").is_some()
        {
            shared
                .cursors
                .remove(&(props.session.clone(), request.into()));
        }
        surface(props, operation)
    }
}
fn surface(props: &PaletteProps, operation: Value) -> Result<CommandOutcome, KernelError> {
    let Some(action) = props.action.clone().with_value(operation) else {
        return Ok(CommandOutcome::Handled { handled: false });
    };
    Ok(CommandOutcome::Surface {
        action,
        fallback: None,
    })
}

fn activate(target: Target, props: &PaletteProps) -> Value {
    match target {
        Target::Command(name) => {
            if folded_typed(&name, props) {
                return json!({"run":props.query.trim()});
            }
            let spec = props
                .commands
                .iter()
                .find(|entry| entry.id == name)
                .and_then(|entry| serde_json::from_value::<CommandSpec>(entry.meta.clone()).ok());
            let exact = props.query.trim().strip_prefix('/').is_some_and(|query| {
                query == name
                    || spec
                        .as_ref()
                        .is_some_and(|spec| spec.aliases.iter().any(|alias| alias == query))
            });
            if matches!(name.as_str(), "permission" | "rename")
                || (!exact
                    && spec.is_some_and(|spec| !spec.opens && !matches!(spec.args, ArgSpec::None)))
            {
                json!({"edit":format!("/{name} ")})
            } else {
                json!({"run":format!("/{name}")})
            }
        }
        Target::Skill(name) => skill(&name, props),
        Target::Key(line) if line.starts_with("@key:") => {
            json!({"key":line.trim_start_matches("@key:")})
        }
        Target::Key(line) | Target::Run(line) => json!({"run":line}),
        Target::Edit(line) => json!({"edit":line}),
        Target::Session(id) | Target::View(id) if id == props.session.as_str() => {
            json!({"message":"this session is already in view"})
        }
        Target::Session(id) => json!({"resume":id}),
        Target::View(id) => json!({"view":id}),
        Target::Inert | Target::Expand(_) => Value::Null,
    }
}

fn folded_typed(destination: &str, props: &PaletteProps) -> bool {
    let Some(name) = props.query.trim().strip_prefix('/') else {
        return false;
    };
    props.commands.iter().any(|entry| {
        entry.meta["foldedInto"].as_str() == Some(destination)
            && (entry.id == name
                || entry.meta["aliases"]
                    .as_array()
                    .is_some_and(|aliases| aliases.iter().any(|alias| alias == name)))
    })
}

/// A skill runs as soon as it is chosen unless it asks for an argument the
/// person has not typed; then its line waits in the box for them.
fn skill(name: &str, props: &PaletteProps) -> Value {
    let asks = props
        .commands
        .iter()
        .find(|entry| entry.id == name)
        .and_then(|entry| serde_json::from_value::<CommandSpec>(entry.meta.clone()).ok())
        .is_some_and(|spec| matches!(spec.args, ArgSpec::Free { hint } if !hint.is_empty()));
    let typed = props.query.trim().strip_prefix('/') == Some(name);
    if asks && !typed {
        json!({"edit":format!("/{name} ")})
    } else {
        json!({"run":format!("/{name}")})
    }
}

/// A row runs a line only the person may run only when the row shows what
/// the line runs: every argument the person has not typed, in order, on the
/// label or beside it.
fn shown(row: Option<&crate::rows::Row>, operation: Value, props: &PaletteProps) -> Value {
    let Some(line) = operation["run"].as_str() else {
        return operation;
    };
    let unseen = unseen(line, &props.query);
    let visible = row.is_some_and(|row| {
        [&row.label, &row.hint]
            .iter()
            .any(|text| holds(text, &unseen))
    });
    if visible || !crate::args::person_only(line, &props.commands) {
        return operation;
    }
    json!({"message":format!("not run: this row does not show what {line} runs; type it yourself")})
}
/// The words of `line` past the ones the person finished typing, or, when
/// the line does not continue what they typed, its whole argument string.
fn unseen<'a>(line: &'a str, query: &str) -> Vec<&'a str> {
    let words = line.split_whitespace().collect::<Vec<_>>();
    let finished = query
        .rsplit_once(char::is_whitespace)
        .map_or(vec![], |(typed, _)| typed.split_whitespace().collect());
    let skip = if !finished.is_empty() && words.starts_with(&finished) {
        finished.len()
    } else {
        1
    };
    words.into_iter().skip(skip).collect()
}
/// Whether `text` holds `words` side by side, in order.
fn holds(text: &str, words: &[&str]) -> bool {
    let said = text.split_whitespace().collect::<Vec<_>>();
    words.is_empty() || said.windows(words.len()).any(|run| run == words)
}

fn operation(
    cursor: &mut crate::Cursor,
    rows: &[&crate::rows::Row],
    target: Option<Target>,
    key: &str,
    props: &PaletteProps,
) -> (bool, Option<Value>) {
    let operation = match key {
        "up" => {
            if let Some(at) = (0..cursor.selected)
                .rev()
                .find(|at| !matches!(rows[*at].target, Target::Inert))
            {
                cursor.selected = at;
            }
            None
        }
        "down" => {
            if let Some(at) = (cursor.selected + 1..rows.len())
                .find(|at| !matches!(rows[*at].target, Target::Inert))
            {
                cursor.selected = at;
            }
            None
        }
        "ctrl+c" => Some(json!({"close":true})),
        "esc" => Some(
            crate::args::parent(&props.query, &props.commands)
                .map(|edit| json!({"edit":edit}))
                .unwrap_or_else(|| json!({"close":true})),
        ),
        "backspace" if props.query.is_empty() || props.query == "/" => Some(json!({"close":true})),
        "tab" | "enter" if matches!(target, Some(Target::Expand(_))) => {
            if let Some(Target::Expand(fold)) = target {
                cursor.expanded.insert(fold);
            }
            None
        }
        "tab" => target.and_then(|target| match target {
            Target::Command(name) | Target::Skill(name) => {
                Some(json!({"edit":format!("/{name} ")}))
            }
            _ => None,
        }),
        "enter" => target
            .filter(|target| !matches!(target, Target::Inert))
            .map(|target| activate(target, props))
            .map(|operation| shown(rows.get(cursor.selected).copied(), operation, props))
            .or_else(|| {
                (props.query.starts_with('/') && props.query.len() > 1)
                    .then(|| json!({"run":props.query.trim()}))
            }),
        _ => return (false, None),
    };
    (true, operation)
}

#[cfg(test)]
pub(crate) mod review_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    pub(crate) fn props() -> PaletteProps {
        serde_json::from_value(json!({"query":"", "open":"keys", "commands":[], "keyHints":{}, "bindings":{}, "session":"s", "conversation":[], "sessions":[], "action":{"name":"client", "args":{}}, "catalogs":{}})).unwrap()
    }
    #[test]
    fn exact_commands_and_aliases_run_while_partial_names_complete() {
        let mut props = props();
        props.commands = vec![CatalogEntry {
            id: "plugins".into(),
            label: "plugins".into(),
            meta: json!({"name":"plugins","hint":"plugins","args":{"kind":"words","values":["enable"],"alone":[]},"instant":true,"family":"session","aliases":["modules"]}),
        }];
        for query in ["/plugins", "/modules"] {
            props.query = query.into();
            assert_eq!(
                activate(Target::Command("plugins".into()), &props),
                json!({"run":"/plugins"})
            );
        }
        props.query = "/plug".into();
        assert_eq!(
            activate(Target::Command("plugins".into()), &props),
            json!({"edit":"/plugins "})
        );
    }
    #[test]
    fn informational_and_interrupt_keys_are_not_actionable() {
        let mut props = props();
        for key in [
            "ctrl+a/e",
            "esc esc",
            "up/down",
            "alt+b/f",
            "1-9 · y/a/n",
            "v · drag",
            "click",
            "esc",
            "ctrl+c",
            "ctrl+d",
        ] {
            props.key_hints.insert(key.into(), "informational".into());
        }
        let (groups, _) = crate::draw::groups(&props, &[], &Default::default());
        assert!(
            groups
                .iter()
                .flat_map(|g| &g.rows)
                .all(|row| !matches!(row.target, Target::Key(_)))
        );
    }
    #[test]
    fn ctrl_c_clears_the_closed_openings_cursor() {
        let home = tempfile::tempdir().unwrap();
        let props = props();
        let state = Arc::new(Mutex::new(crate::State {
            uses: crate::uses::Uses::load(crate::uses::path(home.path())),
            cursors: Default::default(),
        }));
        state.lock().unwrap().cursors.insert(
            (props.session.clone(), "opening".into()),
            Default::default(),
        );
        let outcome = Handler(state.clone())
            .press("opening", "ctrl+c", &props)
            .unwrap();
        assert!(
            matches!(outcome, CommandOutcome::Surface { action, .. } if action.args["value"]["close"] == true)
        );
        assert!(state.lock().unwrap().cursors.is_empty());
    }
    /// A press names the query it was made at and nothing else; the handler
    /// judges it against the props that drawing came from.
    #[test]
    fn a_press_is_judged_against_the_drawing_of_its_own_query() {
        let home = tempfile::tempdir().unwrap();
        let mut props = props();
        props.query = "/mo".into();
        let (groups, selected) = crate::draw::groups(&props, &[], &Default::default());
        let View::Box { layout, .. } = crate::draw::tree(
            &props,
            &groups,
            selected,
            "opening",
            &render::Viewport {
                place: None,
                columns: 80,
                rows: 10,
            },
        ) else {
            panic!("the palette draws a box")
        };
        assert_eq!(
            layout.on_key.unwrap().args,
            json!({"handler":"opening","event":"press","query":"/mo"})
        );
        let state = Arc::new(Mutex::new(crate::State {
            uses: crate::uses::Uses::load(crate::uses::path(home.path())),
            cursors: Default::default(),
        }));
        state.lock().unwrap().cursors.insert(
            (props.session.clone(), "opening".into()),
            crate::Cursor {
                query: props.query.clone(),
                selected,
                drawn: Some(Arc::new(props.clone())),
                expanded: Default::default(),
            },
        );
        let handler = Handler(state);
        let drawn = handler.drawn(&props.session, "opening", &json!("/mo"));
        assert_eq!(drawn.as_deref(), Some(&props));
        assert!(
            handler
                .drawn(&props.session, "opening", &json!("/m"))
                .is_none(),
            "a press made before the last drawing is not judged against it"
        );
        assert!(
            handler
                .drawn(&props.session, "elsewhere", &json!("/mo"))
                .is_none()
        );
    }
    #[test]
    fn arrow_navigation_skips_inert_rows_and_enter_releases_its_cursor() {
        let home = tempfile::tempdir().unwrap();
        let mut props = props();
        for key in ["shift+tab", "esc", "ctrl+o", "ctrl+c"] {
            props.key_hints.insert(key.into(), key.into());
        }
        let state = Arc::new(Mutex::new(crate::State {
            uses: crate::uses::Uses::load(crate::uses::path(home.path())),
            cursors: Default::default(),
        }));
        let handler = Handler(state.clone());
        handler.press("opening", "down", &props).unwrap();
        let outcome = handler.press("opening", "enter", &props).unwrap();
        assert!(
            matches!(outcome, CommandOutcome::Surface { action, .. } if action.args["value"]["key"] == "ctrl+o")
        );
        assert!(state.lock().unwrap().cursors.is_empty());
    }
    fn skill(name: &str, hint: &str) -> CatalogEntry {
        CatalogEntry {
            id: name.into(),
            label: name.into(),
            meta: json!({"name":name,"hint":"a skill","family":"skill/project","args":{"kind":"free","hint":hint}}),
        }
    }
    /// Every skill is reachable from a bare `/`: Enter opens the fold the
    /// cursor rests on, and a skill without an argument runs at once.
    #[test]
    fn enter_unfolds_the_skills_and_runs_one_that_asks_nothing() {
        let home = tempfile::tempdir().unwrap();
        let mut props = props();
        props.open = bingo_sdk::component::PaletteOpen::Commands;
        props.query = "/".into();
        props.commands = vec![skill("deploy", ""), skill("standup", "[days]")];
        let state = Arc::new(Mutex::new(crate::State {
            uses: crate::uses::Uses::load(crate::uses::path(home.path())),
            cursors: Default::default(),
        }));
        let handler = Handler(state.clone());
        let outcome = handler.press("opening", "enter", &props).unwrap();
        assert_eq!(
            outcome,
            CommandOutcome::Handled { handled: true },
            "unfolded"
        );
        let unfolded = state
            .lock()
            .unwrap()
            .cursors
            .values()
            .next()
            .unwrap()
            .expanded
            .clone();
        assert!(unfolded.contains(crate::skills::SKILLS));
        let outcome = handler.press("opening", "enter", &props).unwrap();
        assert!(
            matches!(&outcome, CommandOutcome::Surface { action, .. } if action.args["value"]["run"] == "/deploy"),
            "{outcome:?}"
        );
        assert_eq!(state.lock().unwrap().uses.top(), ["deploy"]);
        // `deploy` is recent now; the fold under it holds the other.
        handler.press("opening", "down", &props).unwrap();
        handler.press("opening", "enter", &props).unwrap();
        let outcome = handler.press("opening", "enter", &props).unwrap();
        assert!(
            matches!(&outcome, CommandOutcome::Surface { action, .. } if action.args["value"]["edit"] == "/standup "),
            "a skill that asks for an argument waits for it: {outcome:?}"
        );
    }
    /// A link that fills the plugin catalogue names the id `evil` "good".
    /// The trust row shows `evil`, and a row that hides it runs nothing.
    #[test]
    fn a_person_only_row_runs_only_the_argument_it_shows() {
        let home = tempfile::tempdir().unwrap();
        let mut props = props();
        props.open = bingo_sdk::component::PaletteOpen::Commands;
        props.commands = vec![CatalogEntry {
            id: "plugins".into(),
            label: "plugins".into(),
            meta: json!({"name":"plugins","hint":"plugins","family":"session","args":{"kind":"words","values":["enable","trust"],"person":["trust"],"alone":[],"then":{"kind":"catalog","source":"plugins"}}}),
        }];
        props.catalogs.insert(
            "plugins".into(),
            vec![CatalogEntry {
                id: "evil".into(),
                label: "good".into(),
                meta: Value::Null,
            }],
        );
        props.query = "/plugins trust ".into();
        let state = Arc::new(Mutex::new(crate::State {
            uses: crate::uses::Uses::load(crate::uses::path(home.path())),
            cursors: Default::default(),
        }));
        let (groups, _) = crate::draw::groups(&props, &[], &Default::default());
        let row = &groups[0].rows[0];
        assert_eq!((row.label.as_str(), row.hint.as_str()), ("evil", "good"));
        let outcome = Handler(state).press("opening", "enter", &props).unwrap();
        assert!(
            matches!(&outcome, CommandOutcome::Surface { action, .. } if action.args["value"]["run"] == "/plugins trust evil"),
            "{outcome:?}"
        );
        let forged = crate::rows::Row {
            label: "good".into(),
            hint: String::new(),
            alert: None,
            now: String::new(),
            marks: vec![],
            hint_marks: vec![],
            target: Target::Run("/plugins trust evil".into()),
        };
        let run = json!({"run":"/plugins trust evil"});
        assert!(shown(Some(&forged), run.clone(), &props)["message"].is_string());
        props.commands[0].meta["args"]["person"] = json!([]);
        assert_eq!(shown(Some(&forged), run.clone(), &props), run);
    }
    /// Every person-only word drawn by a label still runs from its row: the
    /// value it runs stands beside the label.
    #[test]
    fn a_labelled_person_only_word_runs_from_its_row() {
        let home = tempfile::tempdir().unwrap();
        let mut props = props();
        props.open = bingo_sdk::component::PaletteOpen::Commands;
        let modes = [
            "default",
            "acceptEdits",
            "plan",
            "bypassPermissions",
            "dontAsk",
        ];
        props.commands = vec![CatalogEntry {
            id: "permission".into(),
            label: "permission".into(),
            meta: json!({"name":"permission","hint":"mode","family":"session","now":"default","args":{
                "kind":"words","values":modes,"person":modes,"alone":modes,
                "labels":{"acceptEdits":"accept edits","bypassPermissions":"bypass","dontAsk":"don't ask"}}}),
        }];
        props.query = "/permission ".into();
        for (at, value) in modes.iter().enumerate() {
            let handler = Handler(Arc::new(Mutex::new(crate::State {
                uses: crate::uses::Uses::load(crate::uses::path(home.path())),
                cursors: Default::default(),
            })));
            for _ in 0..at {
                handler.press("opening", "down", &props).unwrap();
            }
            let outcome = handler.press("opening", "enter", &props).unwrap();
            assert!(
                matches!(&outcome, CommandOutcome::Surface { action, .. }
                    if action.args["value"]["run"] == format!("/permission {value}")),
                "{value}: {outcome:?}"
            );
        }
    }
    /// A row that shows only the tier runs nothing: the name it trusts is
    /// part of what it runs, and so is the verb nobody typed.
    #[test]
    fn a_person_only_row_must_show_every_argument_not_typed() {
        let mut props = props();
        props.commands = vec![CatalogEntry {
            id: "plugins".into(),
            label: "plugins".into(),
            meta: json!({"name":"plugins","hint":"plugins","family":"session","args":{"kind":"words","values":["approve"],"person":["approve"]}}),
        }];
        let row = |label: &str, hint: &str| crate::rows::Row {
            label: label.into(),
            hint: hint.into(),
            alert: None,
            now: String::new(),
            marks: vec![],
            hint_marks: vec![],
            target: Target::Run("/plugins approve evil full".into()),
        };
        let run = json!({"run":"/plugins approve evil full"});
        let refused = |row, props: &PaletteProps| {
            shown(Some(&row), run.clone(), props)["message"].is_string()
        };
        props.query = "/plugins approve ".into();
        assert!(refused(row("full", ""), &props));
        assert!(refused(row("evil", "full"), &props));
        assert!(!refused(row("evil full", ""), &props));
        props.query = "/plugins approve evil ".into();
        assert!(!refused(row("full", ""), &props));
        props.query = "/pl".into();
        assert!(refused(row("evil full", ""), &props));
        assert!(!refused(row("x", "/plugins approve evil full"), &props));
        props.query = "/plugins\tapprove  evil ".into();
        assert!(!refused(row("full", ""), &props));
    }
    /// The palette reads a person-only line the way its owner does.
    #[test]
    fn a_person_only_line_is_read_at_any_whitespace() {
        let commands = vec![CatalogEntry {
            id: "plugins".into(),
            label: "plugins".into(),
            meta: json!({"name":"plugins","hint":"plugins","family":"session","args":{"kind":"words","values":["trust"],"person":["trust"]}}),
        }];
        for line in [
            "/plugins trust x full",
            "/plugins\ttrust x full",
            "/plugins  trust  x full",
        ] {
            assert!(crate::args::person_only(line, &commands), "{line:?}");
        }
        assert!(!crate::args::person_only("/plugins\tlist", &commands));
    }
    #[test]
    fn enter_runs_a_command_that_opens_and_lists_the_arguments_of_one_that_does_not() {
        let mut props = props();
        for opens in [true, false] {
            let name = if opens { "model" } else { "permission" };
            props.commands.push(CatalogEntry {
                id: name.into(),
                label: String::new(),
                meta: json!({"name":name,"hint":"","family":"session","opens":opens,"args":{"kind":"catalog","source":"models"}}),
            });
        }
        assert_eq!(
            activate(Target::Command("model".into()), &props),
            json!({"run":"/model"})
        );
        assert_eq!(
            activate(Target::Command("permission".into()), &props),
            json!({"edit":"/permission "})
        );
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    fn props(query: &str) -> PaletteProps {
        let mut props = super::review_tests::props();
        props.open = component::PaletteOpen::Commands;
        props.query = query.into();
        props.commands = [
            ("crew", None),
            ("agents", Some("crew")),
            ("room", Some("crew")),
            ("permission", None),
            ("rename", None),
        ]
        .map(|(name, into)| CatalogEntry {
            id: name.into(),
            label: name.into(),
            meta: json!({"name":name,"hint":"fixture","family":"session",
                    "foldedInto":into,"aliases":if name == "room" {vec!["rooms"]} else {vec![]},
                    "args":if name == "rename" {json!({"kind":"free","hint":"session name"})}
                        else {json!({"kind":"words","values":["default","acceptEdits","plan"]})}}),
        })
        .to_vec();
        props
    }

    fn enter(query: &str) -> Value {
        let props = props(query);
        let (groups, selected) = crate::draw::groups(&props, &[], &Default::default());
        let rows = groups
            .iter()
            .flat_map(|group| &group.rows)
            .collect::<Vec<_>>();
        let target = rows.get(selected).map(|row| row.target.clone());
        let mut cursor = crate::Cursor {
            selected,
            ..Default::default()
        };
        operation(&mut cursor, &rows, target, "enter", &props)
            .1
            .unwrap()
    }

    #[test]
    fn a_fully_typed_folded_name_dispatches_its_original_command() {
        for line in [
            "/agents",
            "/room",
            "/rooms",
            "/agents stop scout",
            "/room close design",
        ] {
            assert_eq!(enter(line), json!({"run":line}));
        }
        assert_eq!(enter("/agen"), json!({"edit":"/crew "}));
    }

    #[test]
    fn bare_permission_and_rename_open_their_inline_levels() {
        for name in ["permission", "rename"] {
            assert_eq!(
                enter(&format!("/{name}")),
                json!({"edit":format!("/{name} ")})
            );
        }
        assert_eq!(
            enter("/rename release notes"),
            json!({"run":"/rename release notes"})
        );
        let mut props = props("/permission ");
        let entry = props
            .commands
            .iter_mut()
            .find(|entry| entry.id == "permission")
            .unwrap();
        entry.meta["args"]["values"] = json!([
            "default",
            "acceptEdits",
            "plan",
            "bypassPermissions",
            "dontAsk"
        ]);
        entry.meta["now"] = json!("plan");
        let (groups, selected) = crate::draw::groups(&props, &[], &Default::default());
        assert_eq!(groups[0].rows.len(), 5);
        assert_eq!(groups[0].rows[selected].label, "plan");
        assert_eq!(groups[0].rows[selected].now, "now");
    }
}
