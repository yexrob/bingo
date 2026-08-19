//! What the instance registry is asserted to be.
//!
//! Lifted out of `agents.rs` whole (D149), split the way
//! `app/controller/tests.rs` already splits its own: the file was over the
//! line and two thirds of it was this.

use super::*;

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn test_session() -> Arc<Session> {
    let core = crate::app::AppCore::start(Default::default());
    Arc::new(Session {
        client: crate::api::client::Client::new("k".into(), "http://x".into()),
        runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
        permission_mode: crate::permission::PermissionMode::Default,
        settings: crate::settings::Settings::default(),
        system: Vec::new(),
        depth: 1,
        cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
        home: std::env::temp_dir(),
        user_config_dir: std::env::temp_dir().join(".config"),
        quiet: true,
        compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        core: core.clone(),
        watch: core.watch(),
        tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
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
    })
}

#[test]
fn list_reports_the_runtime_engine() {
    let registry = crate::app::AppCore::start(Default::default()).agents();
    let session = test_session();
    let _ = session.runtime.model_tx.send("gpt-5.6-sol".into());
    let _ = session.runtime.provider_tx.send("road".into());
    registry
        .insert(
            "dev",
            AgentKind::Hire,
            None,
            "implementation".into(),
            session,
        )
        .now();

    let statuses = registry.list();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].model, "gpt-5.6-sol");
    assert_eq!(statuses[0].provider, "road");
}

#[test]
fn loads_defs_with_project_over_user_precedence() {
    let root = std::env::temp_dir().join(format!("bingo-agents-{}-load", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let project = root.join("project");
    write(
        &home.join(".config/bingo/agents/reviewer.md"),
        "---\ndescription: user reviewer\nmodel: haiku\n---\nYou are the reviewer.\n",
    );
    write(
        &project.join(".bingo/agents/reviewer.md"),
        "---\ndescription: project reviewer\n---\nYou are the project reviewer.\n",
    );
    write(&project.join(".bingo/agents/scout.md"), "For research.\n");
    let defs = load_agent_defs(&home, &project);
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["reviewer", "scout"],
        "the project layer overrides the user layer for same names"
    );
    let reviewer = &defs[0];
    assert_eq!(reviewer.description, "project reviewer");
    assert!(reviewer.system.contains("project reviewer"));
    assert!(
        reviewer.model.is_none(),
        "the overridden user definition does not leak through"
    );
    assert_eq!(
        reviewer.source,
        AgentDefSource::Project,
        "a cross-layer same-name override takes the project source"
    );
    // No frontmatter: name comes from the file name, description falls back to the first body line.
    assert_eq!(defs[1].description, "For research.");
    assert_eq!(defs[1].source, AgentDefSource::Project);
    let _ = std::fs::remove_dir_all(&root);
}

/// source=User when only the user layer has a definition (D31 badge data).
#[test]
fn source_is_user_when_only_user_layer_has_def() {
    let root = std::env::temp_dir().join(format!("bingo-agents-{}-src", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    write(
        &home.join(".config/bingo/agents/only-user.md"),
        "User-layer only.\n",
    );
    let defs = load_agent_defs(&home, &root);
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "only-user");
    assert_eq!(defs[0].source, AgentDefSource::User);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn frontmatter_name_and_model_override() {
    let root = std::env::temp_dir().join(format!("bingo-agents-{}-fm", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    write(
        &home.join(".config/bingo/agents/x.md"),
        "---\nname: deep-dive\ndescription: >-\n  multi-line\n  description\nmodel: sub-model\nprovider: ds\nthinking: xhigh\n---\nsystem body\n",
    );
    let defs = load_agent_defs(&home, &root);
    assert_eq!(defs.len(), 1);
    assert_eq!(
        defs[0].name, "deep-dive",
        "frontmatter name overrides the file name"
    );
    assert_eq!(
        defs[0].description, "multi-line description",
        "folded scalar"
    );
    assert_eq!(defs[0].model.as_deref(), Some("sub-model"));
    assert_eq!(defs[0].provider.as_deref(), Some("ds"));
    assert_eq!(defs[0].thinking.as_deref(), Some("xhigh"));
    assert_eq!(defs[0].system, "system body");
    assert!(
        defs[0].inherit_system,
        "defaults to appending to the parent system"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `inherit_system: false` opts into replacing the parent's system blocks; anything else
/// (including a typo) keeps the safe default.
#[test]
fn frontmatter_inherit_system_opt_out() {
    let root = std::env::temp_dir().join(format!("bingo-agents-{}-inherit", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    write(
        &home.join(".config/bingo/agents/lean.md"),
        "---\nname: lean\ninherit_system: false\n---\npersona only\n",
    );
    write(
        &home.join(".config/bingo/agents/keep.md"),
        "---\nname: keep\ninherit_system: yes\n---\nappended as usual\n",
    );
    let defs = load_agent_defs(&home, &root);
    let by = |n: &str| defs.iter().find(|d| d.name == n).unwrap().inherit_system;
    assert!(!by("lean"));
    assert!(by("keep"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn claim_name_dedupes_and_defaults() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    assert_eq!(reg.claim_name("").now(), "agent", "empty name falls back");
    assert_eq!(reg.claim_name("reviewer").now(), "reviewer");
    reg.insert(
        "reviewer",
        AgentKind::Hire,
        None,
        "r".into(),
        test_session(),
    )
    .now();
    assert_eq!(reg.claim_name("reviewer").now(), "reviewer-2");
    reg.insert(
        "reviewer-2",
        AgentKind::Hire,
        None,
        "r".into(),
        test_session(),
    )
    .now();
    assert_eq!(reg.claim_name("reviewer").now(), "reviewer-3");
}

/// A hire serves one task and goes (D53): once it is idle with nothing waiting, the
/// lease runs out and the name is released. The crew is never touched, and main gets
/// one round to follow up before the instance disappears under it.
#[test]
fn a_finished_hire_is_released_and_the_crew_is_not() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "dev",
        AgentKind::Crew,
        None,
        "member".into(),
        test_session(),
    )
    .now();
    reg.insert(
        "temp",
        AgentKind::Hire,
        None,
        "one job".into(),
        test_session(),
    )
    .now();
    // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
    // and a hire with no run behind it is unstarted, not finished.
    let _ = reg.next_run("temp").now();

    // Running: nothing is released, however many sweeps run.
    assert!(reg.release_hires().now().is_empty());
    assert!(reg.release_hires().now().is_empty());
    assert_eq!(reg.list().len(), 2, "a working hire keeps its name");

    // Finished: idle, empty inbox, nothing owed. One sweep is not enough — that would
    // take the instance away in the very round its result reaches main.
    assert!(reg.finish("temp", Vec::new(), 1).now().is_none());
    assert!(
        reg.release_hires().now().is_empty(),
        "main still has a round to follow up in"
    );
    assert_eq!(reg.release_hires().now(), vec!["temp".to_string()]);
    let left = reg.list();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "dev", "the crew member is untouched");
    assert_eq!(left[0].kind, AgentKind::Crew);
}

/// A follow-up renews the lease: the hire main is still talking to is not swept out
/// from under the conversation.
#[test]
fn a_hire_with_work_waiting_keeps_its_name() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "dev",
        AgentKind::Crew,
        None,
        "member".into(),
        test_session(),
    )
    .now();
    reg.insert(
        "temp",
        AgentKind::Hire,
        None,
        "one job".into(),
        test_session(),
    )
    .now();
    // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
    // and a hire with no run behind it is unstarted, not finished.
    let _ = reg.next_run("temp").now();
    assert!(reg.finish("temp", Vec::new(), 1).now().is_none());
    assert!(reg.release_hires().now().is_empty());

    // A queued follow-up is work waiting: the count goes back to full.
    let _ = reg
        .deliver(
            "temp",
            crate::channels::MAIN_NAME,
            "one more thing",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(reg.release_hires().now().is_empty());
    assert!(
        reg.release_hires().now().is_empty(),
        "the lease was renewed"
    );

    // Read into a run and answered, with nothing left waiting → released as before.
    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1);
    assert!(reg.finish("temp", Vec::new(), 1).now().is_none());
    assert!(reg.release_hires().now().is_empty());
    assert_eq!(reg.release_hires().now(), vec!["temp".to_string()]);
}

/// A message the hire never answered is not a finished task. Releasing there would
/// destroy the record the sender uses to find out it was left hanging.
#[test]
fn a_hire_still_owing_an_answer_is_not_released() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "dev",
        AgentKind::Crew,
        None,
        "member".into(),
        test_session(),
    )
    .now();
    reg.insert(
        "temp",
        AgentKind::Hire,
        None,
        "one job".into(),
        test_session(),
    )
    .now();
    // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
    // and a hire with no run behind it is unstarted, not finished.
    let _ = reg.next_run("temp").now();
    assert!(reg.finish("temp", Vec::new(), 1).now().is_none());
    let _ = reg
        .deliver(
            "temp",
            crate::channels::MAIN_NAME,
            "answer me",
            Vec::new(),
            None,
        )
        .now();
    assert_eq!(
        reg.flush_pending().now().len(),
        1,
        "the idle hire takes the message"
    );
    // The run that read it ended saying nothing: delivered, unanswered, inbox empty.
    assert!(reg.finish("temp", Vec::new(), 0).now().is_none());
    let owed = |reg: &AgentHandle| {
        reg.list()
            .into_iter()
            .find(|a| a.name == "temp")
            .map(|a| a.unacked)
            .unwrap_or_default()
    };
    assert_eq!(owed(&reg), 1);
    for _ in 0..4 {
        assert!(
            reg.release_hires().now().is_empty(),
            "an outstanding message holds the instance open"
        );
    }
}

/// Without a crew there is nothing for a hire to be temporary *relative to*: an ad-hoc
/// subagent is the ordinary way to work in such a project, and sweeping it would delete
/// instances main still expects to address.
#[test]
fn hires_are_not_swept_in_a_project_with_no_crew() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "temp",
        AgentKind::Hire,
        None,
        "one job".into(),
        test_session(),
    )
    .now();
    // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
    // and a hire with no run behind it is unstarted, not finished.
    let _ = reg.next_run("temp").now();
    assert!(reg.finish("temp", Vec::new(), 1).now().is_none());
    for _ in 0..4 {
        assert!(reg.release_hires().now().is_empty());
    }
    assert_eq!(reg.list().len(), 1);

    // A crew that has been stopped is not a crew either.
    reg.insert(
        "dev",
        AgentKind::Crew,
        None,
        "member".into(),
        test_session(),
    )
    .now();
    let _ = reg.stop("dev").now();
    for _ in 0..4 {
        assert!(reg.release_hires().now().is_empty());
    }
}

/// A stopped hire will never run again, so it goes on the spot rather than waiting out
/// a lease that measures a follow-up window it can no longer receive.
#[test]
fn a_stopped_hire_is_released_immediately() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "dev",
        AgentKind::Crew,
        None,
        "member".into(),
        test_session(),
    )
    .now();
    reg.insert(
        "temp",
        AgentKind::Hire,
        None,
        "one job".into(),
        test_session(),
    )
    .now();
    // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
    // and a hire with no run behind it is unstarted, not finished.
    let _ = reg.next_run("temp").now();
    let _ = reg.stop("temp").now();
    assert_eq!(reg.release_hires().now(), vec!["temp".to_string()]);
    assert_eq!(reg.list().len(), 1);
}

#[test]
fn activity_timestamp_refreshes_only_when_the_instance_is_active() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        test_session(),
    )
    .now();
    let initial = reg.list()[0].last_active;
    std::thread::sleep(Duration::from_millis(2));
    let unchanged = reg.list()[0].last_active;
    assert_eq!(unchanged, initial, "listing is not agent activity");

    let first = reg
        .deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "add A",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    let delivered = reg.list()[0].last_active;
    assert!(
        delivered > initial,
        "receiving an inbox message is activity"
    );

    std::thread::sleep(Duration::from_millis(2));
    let _ = reg.follow_up("scout", first).now();
    assert_eq!(
        reg.list()[0].last_active,
        delivered,
        "watchdog bookkeeping is not agent activity"
    );

    std::thread::sleep(Duration::from_millis(2));
    reg.touch("scout");
    reg.settle_now();
    let streamed = reg.list()[0].last_active;
    assert!(
        streamed > delivered,
        "stream and tool hooks can touch the entry"
    );

    std::thread::sleep(Duration::from_millis(2));
    assert!(reg.finish("scout", Vec::new(), 0).now().is_some());
    let finished = reg.list()[0].last_active;
    assert!(finished > streamed, "turn completion is activity");
}

/// `replace_history` is the one call that irreversibly rewrites an
/// instance's context (`/compact` on its page, D135), and its safety
/// argument is a state check under the same lock as the write: a run that
/// starts in between loses the race instead of losing the work. Nothing
/// pinned that (D135a).
#[test]
fn replace_history_refuses_a_running_instance_and_drops_stale_clocks() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        test_session(),
    )
    .now();
    assert!(
        !reg.replace_history("ghost", vec![Message::user_text("summary")])
            .now(),
        "an instance that is not there cannot be rewritten"
    );
    // `insert` is the spawn path, and a spawned instance is running.
    assert!(
        !reg.replace_history("scout", vec![Message::user_text("summary")])
            .now(),
        "a running instance holds its own copy and overwrites this at finish"
    );

    assert!(
        reg.finish(
            "scout",
            vec![
                Message::user_text("a"),
                Message::user_text("b"),
                Message::user_text("c"),
            ],
            0,
        )
        .now()
        .is_none()
    );
    assert!(
        reg.replace_history("scout", vec![Message::user_text("summary")])
            .now(),
        "idle, so the rewrite lands"
    );
    let (history, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
    assert_eq!(history.len(), 1);
    assert_eq!(
        stamps,
        vec![0],
        "a shorter history's old clocks no longer describe it: no stamp beats a wrong one"
    );
}

/// Every stored history message carries a landing clock for the DM view;
/// a rewritten (shorter) history drops the stale clocks instead of lying.
#[test]
fn finish_stamps_history_and_drops_clocks_on_rewrite() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        test_session(),
    )
    .now();
    assert!(
        reg.finish(
            "scout",
            vec![Message::user_text("a"), Message::user_text("b")],
            0,
        )
        .now()
        .is_none()
    );
    let (history, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
    assert_eq!(stamps.len(), history.len());
    assert!(stamps.iter().all(|&at| at > 0), "{stamps:?}");
    // Compaction hands back a shorter, rewritten history: the old clocks no
    // longer describe it — no stamp is rendered rather than a wrong one.
    assert!(
        reg.finish("scout", vec![Message::user_text("summary")], 0)
            .now()
            .is_none()
    );
    let (history, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
    assert_eq!(history.len(), 1);
    assert_eq!(stamps, vec![0]);
    // The record grows again: only the new tail is stamped.
    assert!(
        reg.finish(
            "scout",
            vec![Message::user_text("summary"), Message::user_text("more")],
            0,
        )
        .now()
        .is_none()
    );
    let (_, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
    assert_eq!(stamps[0], 0, "the rewritten prefix stays clockless");
    assert!(stamps[1] > 0, "{stamps:?}");
}

/// A message claimed by a run must not vanish between the drain and the
/// landing: the inbox empties at the claim point and the history only
/// catches up at finish — the in-flight record carries the DM view across
/// that window, and lets go the moment the history holds the message.
#[test]
fn a_claimed_message_stays_visible_until_it_lands() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        test_session(),
    )
    .now();
    assert!(reg.finish("scout", Vec::new(), 0).now().is_none());
    let _ = reg
        .deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "map the module",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        reg.pending_of("scout"),
        vec![(
            crate::channels::MAIN_NAME.to_string(),
            "map the module".to_string()
        )],
        "the sender rides with the message: a pair view has one conversation in it"
    );

    // Claimed: gone from the inbox, not yet in the history — in flight.
    assert_eq!(reg.flush_pending().now().len(), 1);
    assert!(reg.pending_of("scout").is_empty());
    let (history, _, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
    let in_flight = reg.in_flight_of("scout");
    assert!(history.is_empty());
    assert_eq!(
        in_flight,
        vec![(
            crate::channels::MAIN_NAME.to_string(),
            "map the module".to_string()
        )]
    );

    // Landed: the stored history carries it, the bridge record is gone.
    let landed = vec![
        Message::user_text("map the module"),
        Message {
            role: crate::api::types::Role::Assistant,
            content: vec![crate::api::types::ContentBlock::Text {
                text: "mapped".into(),
            }],
        },
    ];
    assert!(reg.finish("scout", landed, 6).now().is_none());
    let (history, _, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
    assert_eq!(history.len(), 2);
    let in_flight = reg.in_flight_of("scout");
    assert!(in_flight.is_empty(), "history took over: {in_flight:?}");
}

/// A failed run puts its claimed batch back in the inbox; the in-flight
/// record lets go of it in the same move, so the message reads as queued
/// again instead of being on screen twice.
#[test]
fn a_restored_message_leaves_the_in_flight_record() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        test_session(),
    )
    .now();
    assert!(reg.finish("scout", Vec::new(), 0).now().is_none());
    let _ = reg
        .deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "map the module",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    let wake = reg
        .flush_pending()
        .now()
        .pop()
        .unwrap_or_else(|| panic!("claimed"));
    let in_flight = reg.in_flight_of("scout");
    assert_eq!(in_flight.len(), 1);

    reg.restore_inbox("scout", wake.items);
    reg.settle_now();
    let in_flight = reg.in_flight_of("scout");
    assert!(in_flight.is_empty(), "{in_flight:?}");
    assert_eq!(
        reg.pending_of("scout"),
        vec![(
            crate::channels::MAIN_NAME.to_string(),
            "map the module".to_string()
        )],
        "the sender rides with the message: a pair view has one conversation in it"
    );
}

#[test]
fn lifecycle_running_idle_queue_and_revive() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        test_session(),
    )
    .now();
    // Running: message queued (delivery never happens inside deliver itself).
    let first = reg
        .deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "add A",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    // Turn finished + inbox non-empty → continues (history saved, inbox drained, ack set).
    let next = reg
        .finish("scout", vec![Message::user_text("hi")], 1)
        .now()
        .unwrap_or_else(|| panic!("should continue"));
    assert_eq!(
        next.history.len(),
        1,
        "the continuation carries the latest history"
    );
    assert!(
        matches!(&next.items[..], [InboxItem::Direct { text: m, .. }] if m == "add A"),
        "inbox content"
    );
    assert_eq!(reg.list()[0].state, AgentState::Running);
    let acks = reg.acks_of("scout").unwrap_or_else(|| unreachable!());
    assert_eq!(acks[0].id, first);
    assert_eq!(acks[0].state, AckState::Delivered { run: next.run });
    // Finish again with an empty inbox → Idle.
    assert!(reg.finish("scout", Vec::new(), 1).now().is_none());
    assert_eq!(reg.list()[0].state, AgentState::Idle);
    // Idle: the message waits for a flush rather than starting a run on the spot.
    let _ = reg
        .deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "look at B again",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        reg.list()[0].state,
        AgentState::Idle,
        "delivery does not start a run by itself"
    );
    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1);
    assert!(
        matches!(&woken[0].items[..], [InboxItem::Direct { text: m, .. }] if m == "look at B again")
    );
    assert_eq!(reg.list()[0].state, AgentState::Running);
    assert!(
        reg.flush_pending().now().is_empty(),
        "claimed instances do not start twice"
    );
}

/// Every producer at once, through the registries: several runs depositing
/// into inboxes and posting into a room while a frontend reads the stream.
///
/// Two things are asserted and they are the two the actor exists for. One
/// **history**: the sequence numbers the events carry are strictly
/// increasing and gapless, however the work interleaved. And **no
/// deadlock**: every producer finishes, because no producer waits to be
/// heard and the actor waits on nobody — the timeout is what makes that an
/// assertion rather than a hope.
#[tokio::test]
async fn concurrent_registry_work_makes_one_gapless_history() {
    use crate::app::command::AppQuery;
    use crate::app::{AppCore, AppFrame, AppReply, AppRequest, AttachRequest, RequestId};
    use crate::channels::ChannelMode;

    const RUNS: usize = 6;
    const EACH: usize = 15;

    let core = AppCore::start(Default::default());
    let agents = core.agents();
    let rooms = core.channels();
    let session = test_session();
    let mut link = core
        .attach(AttachRequest::new("test"))
        .unwrap_or_else(|error| panic!("{error}"));
    link.request(AppRequest::Query {
        id: RequestId(1),
        query: AppQuery::ReadSession,
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let cursor = match link.recv().await {
        Some(AppFrame::Reply {
            result: Ok(AppReply::Session(snapshot)),
            ..
        }) => snapshot.event_cursor,
        other => panic!("expected a session snapshot, got {other:?}"),
    };

    let names: Vec<String> = (0..RUNS).map(|i| format!("w{i}")).collect();
    for name in &names {
        agents
            .insert(name, AgentKind::Hire, None, "w".into(), session.clone())
            .await;
    }
    rooms
        .create("build", names.clone(), ChannelMode::Free)
        .await
        .unwrap_or_else(|error| panic!("{error}"));

    let mut runs = Vec::new();
    for name in names.clone() {
        let agents = agents.clone();
        let rooms = rooms.clone();
        runs.push(tokio::spawn(async move {
            for step in 0..EACH {
                agents.deposit(&name, room_line("peer", step as u64)).await;
                let _ = rooms.post(&name, "build", "line").await;
            }
        }));
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        for run in runs {
            run.await.unwrap_or_else(|error| panic!("{error}"));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("a producer never finished: the actor stopped serving"));

    // Everything published up to here is already queued on the attachment.
    let mut seen = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(200), link.recv()).await
    {
        match frame {
            AppFrame::Event(event) => seen.push(event.meta.seq),
            other => panic!("expected an event, got {other:?}"),
        }
    }
    assert!(
        seen.len() >= RUNS * EACH,
        "every post and every deposit is accounted for: {} events",
        seen.len()
    );
    assert_eq!(
        seen,
        (cursor + 1..=cursor + seen.len() as u64).collect::<Vec<_>>(),
        "one history: strictly increasing, gapless, in arrival order"
    );
}

fn room_line(from: &str, seq: u64) -> InboxItem {
    InboxItem::Channel {
        channel: "t".into(),
        from: from.into(),
        text: "report".into(),
        seq,
    }
}

#[test]
fn inbox_accumulates_direct_and_channel_items_in_order() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    let _ = reg
        .deliver(
            "w",
            crate::channels::MAIN_NAME,
            "do 1 first",
            Vec::new(),
            None,
        )
        .now();
    assert!(reg.deposit("w", room_line("a", 3)).now());
    let items = reg
        .finish("w", Vec::new(), 1)
        .now()
        .unwrap_or_else(|| panic!("continue"))
        .items;
    assert_eq!(items.len(), 2);
    assert!(
        matches!(&items[0], InboxItem::Direct { text: m, .. } if m == "do 1 first"),
        "in order"
    );
    assert!(
        matches!(&items[1], InboxItem::Channel { seq: 3, from, .. } if from == "a"),
        "channel entries carry seq/from"
    );
    // v7: an unmentioned room line wakes an idle member like any other —
    // the `@` decides what is owed, never what is read. A finished turn
    // with an empty inbox still parks idle: nothing polls.
    assert!(
        reg.finish("w", Vec::new(), 1).now().is_none(),
        "empty inbox parks"
    );
    assert!(reg.deposit("w", room_line("b", 4)).now());
    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1, "one unmentioned line is enough");
    assert_eq!(woken[0].items.len(), 1);
    assert!(reg.finish("w", Vec::new(), 1).now().is_none());
    assert!(reg.deposit("w", room_line("b", 5)).now());
    assert!(reg.deposit("w", room_line("b", 6)).now());
    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1);
    assert_eq!(
        woken[0].items.len(),
        2,
        "whatever is waiting drains together, in order"
    );
    let _ = reg.stop("w").now();
    let dropped = room_line("c", 6);
    assert!(
        !reg.deposit("w", dropped.clone()).now(),
        "stopped members do not receive"
    );
    assert!(
        !reg.deposit("ghost", dropped).now(),
        "unknown instances are silently dropped"
    );
}

/// v7's wake rule, whole: a non-empty inbox wakes, an empty one never
/// does. The count and age gates it replaces were proxies for a question
/// the sender now answers with the `@` — and in a room of six the count
/// was an amplifier, since one round where everyone speaks leaves five
/// unread in every inbox and re-crosses it for all of them.
#[test]
fn any_waiting_line_wakes_and_an_empty_inbox_never_does() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    assert!(reg.finish("w", Vec::new(), 0).now().is_none(), "start idle");
    assert!(
        reg.flush_pending().now().is_empty(),
        "an empty inbox never wakes: no polling, and a quiet room is free"
    );

    assert!(reg.deposit("w", room_line("a", 1)).now());
    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1, "one unmentioned line wakes on its own");
    assert_eq!(woken[0].items.len(), 1);
    assert!(
        reg.flush_pending().now().is_empty(),
        "and the drain leaves nothing behind to wake it again"
    );

    // A running member is not woken twice: what lands while it works is
    // absorbed at its next tool boundary instead (`take_running`).
    assert!(reg.deposit("w", room_line("a", 2)).now());
    assert!(
        reg.flush_pending().now().is_empty(),
        "a running member takes its mail at the tool boundary, not by waking"
    );
    assert_eq!(reg.take_running("w", 0).now().len(), 1, "steered mid-turn");
}

#[test]
fn share_hooks_track_insert_finish_stop() {
    let root = std::env::temp_dir().join(format!("bingo-agents-{}-share", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = crate::share::ShareStore::load_or_create(&root.join("shares").join("s.json"))
        .unwrap_or_else(|e| panic!("{e}"));
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.attach_share(store.clone());

    // insert → creates an entry (running, empty history).
    reg.insert(
        "scout",
        AgentKind::Hire,
        Some("scout".into()),
        "research".into(),
        test_session(),
    )
    .now();
    let doc = store.snapshot();
    assert_eq!(doc.agents.len(), 1);
    assert_eq!(doc.agents[0].state, "running");
    assert_eq!(doc.agents[0].def.as_deref(), Some("scout"));
    assert!(doc.agents[0].history.is_empty());

    // finish → history + state (empty inbox → idle).
    reg.finish("scout", vec![Message::user_text("hi")], 1).now();
    let doc = store.snapshot();
    assert_eq!(doc.agents[0].state, "idle");
    assert_eq!(doc.agents[0].history.len(), 1);
    assert_eq!(doc.agents[0].history[0], Message::user_text("hi"));

    // A busy non-empty inbox → stays running after finish (Idle wake-up drains the inbox into Start,
    // while Running queues; two instructions create the queue scenario).
    reg.deliver(
        "scout",
        crate::channels::MAIN_NAME,
        "check again",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("{e}"));
    reg.deliver(
        "scout",
        crate::channels::MAIN_NAME,
        "check once more",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("{e}"));
    reg.finish("scout", Vec::new(), 1).now();
    let doc = store.snapshot();
    assert_eq!(doc.agents[0].state, "running");
    // Inbox drained → idle.
    reg.finish("scout", Vec::new(), 1).now();
    let doc = store.snapshot();
    assert_eq!(doc.agents[0].state, "idle");

    // stop → stopped.
    reg.stop("scout").now().unwrap_or_else(|e| panic!("{e}"));
    let doc = store.snapshot();
    assert_eq!(doc.agents[0].state, "stopped");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn main_name_is_reserved() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    assert_eq!(
        reg.claim_name("main").now(),
        "main-2",
        "the main agent owns the name; a subagent asking for it is renamed"
    );
}

/// Several messages sent before a boundary arrive as one batch: the receiver reads them
/// together instead of burning a turn per message.
#[test]
fn messages_sent_in_one_turn_arrive_as_one_batch() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    assert!(
        reg.finish("w", Vec::new(), 1).now().is_none(),
        "turns idle first"
    );
    for text in ["look at A first", "look at B again", "and finally C"] {
        reg.deliver("w", crate::channels::MAIN_NAME, text, Vec::new(), None)
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
    }
    assert_eq!(
        reg.list()[0].pending,
        3,
        "all queued, none started individually"
    );

    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1, "one instance runs one round");
    assert_eq!(woken[0].items.len(), 3, "all three delivered at once");
    let acks = reg.acks_of("w").unwrap_or_else(|| unreachable!());
    assert!(
        acks.iter()
            .all(|a| a.state == AckState::Delivered { run: woken[0].run }),
        "all three land in one round: {acks:?}"
    );
}

/// Stopping discards the inbox — every message in it is recorded as dropped, so a sender
/// that only saw "queued" can still find out it was never delivered.
#[test]
fn stop_records_undelivered_messages_as_dropped() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    let id = reg
        .deliver(
            "w",
            crate::channels::MAIN_NAME,
            "is it too late",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    let (_, dropped) = reg.stop("w").now().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(dropped, 1);
    let acks = reg.acks_of("w").unwrap_or_else(|| unreachable!());
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].id, id);
    assert!(
        matches!(&acks[0].state, AckState::Dropped { reason } if reason.contains("stopped")),
        "{:?}",
        acks[0].state
    );
    assert_eq!(reg.list()[0].pending, 0, "inbox cleared");
}

/// CC subagent semantics (D105a): a direct message to a stopped instance
/// resumes it — the registry kept its session and history, so the delivery
/// flips it to idle and the ordinary wake path takes it from there. The
/// chase and the room broadcast deliberately do not take this door.
#[test]
fn a_direct_message_resumes_a_stopped_instance() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    reg.finish("w", vec![crate::api::types::Message::user_text("go")], 0)
        .now();
    reg.stop("w").now().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reg.list()[0].state, AgentState::Stopped);

    reg.deliver(
        "w",
        crate::channels::MAIN_NAME,
        "carry on",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("a stopped instance accepts a direct message: {e}"));
    assert_eq!(
        reg.list()[0].state,
        AgentState::Idle,
        "flipped, not refused"
    );

    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1, "the ordinary wake path picks it up");
    assert_eq!(woken[0].name, "w");
    assert_eq!(
        woken[0].history,
        vec![crate::api::types::Message::user_text("go")],
        "resumed from the history the registry kept"
    );
    assert_eq!(reg.list()[0].state, AgentState::Running);
}

/// The chase is bounded and self-cancelling: while a message goes unanswered each round leaves
/// one follow-up riding with it, the budget stops at MAX_FOLLOW_UPS, and the reply that finally
/// comes settles every later check.
#[test]
fn follow_up_chases_a_queued_message_until_the_budget_runs_out() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    let id = reg
        .deliver(
            "w",
            crate::channels::MAIN_NAME,
            "check the logs",
            Vec::new(),
            Some(Duration::from_secs(30)),
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    for round in 1..=MAX_FOLLOW_UPS {
        assert_eq!(reg.follow_up("w", id).now(), FollowUp::Sent { round });
    }
    assert_eq!(
        reg.follow_up("w", id).now(),
        FollowUp::Exhausted,
        "budget exhausted"
    );
    let items = reg
        .finish("w", Vec::new(), 1)
        .now()
        .unwrap_or_else(|| panic!("queued messages should be claimed by the receiver"))
        .items;
    assert_eq!(
        items.len(),
        1 + MAX_FOLLOW_UPS as usize,
        "follow-ups arrive in the same batch as the original"
    );
    assert!(
        matches!(&items[1], InboxItem::FollowUp { original, round: 1, .. } if *original == id),
        "follow-up points at the original message: {:?}",
        items[1]
    );
    let acks = reg.acks_of("w").unwrap_or_else(|| unreachable!());
    assert_eq!(acks.len(), 1, "the follow-up itself leaves no receipt");
    assert_eq!(
        acks[0].follow_ups, MAX_FOLLOW_UPS,
        "follow-up count is available for review"
    );
    assert_eq!(acks[0].timeout, Some(Duration::from_secs(30)));
    // Read into a prompt is still not an acknowledgement — only the reply ends the chase.
    assert!(
        matches!(
            reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Delivered { .. }
        ),
        "entering the context is not yet a receipt"
    );
    assert!(
        reg.finish("w", Vec::new(), 2).now().is_none(),
        "that round answers"
    );
    assert!(
        matches!(
            reg.follow_up("w", id).now(),
            FollowUp::Settled(AckState::Answered { .. })
        ),
        "no follow-up after a reply"
    );
}

/// The silence the sender actually cares about: the receiver took the message and ended its
/// turn without a word. Delivery looks like success and is not, so the chase must continue —
/// and the follow-up has to name which silence it is, since the two need different words.
#[test]
fn a_turn_that_says_nothing_does_not_acknowledge_what_it_read() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert(
        "mute",
        AgentKind::Hire,
        None,
        "silent".into(),
        test_session(),
    )
    .now();
    assert!(
        reg.finish("mute", Vec::new(), 1).now().is_none(),
        "turns idle first"
    );
    let id = reg
        .deliver(
            "mute",
            crate::channels::MAIN_NAME,
            "report progress",
            Vec::new(),
            Some(Duration::from_secs(30)),
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        reg.flush_pending().now().len(),
        1,
        "idle instances receive at the boundary"
    );
    // The turn ends producing no text for main.
    assert!(reg.finish("mute", Vec::new(), 0).now().is_none());
    let acks = reg.acks_of("mute").unwrap_or_else(|| unreachable!());
    assert!(
        matches!(acks[0].state, AckState::Delivered { run: 1 }),
        "a silent round is not a receipt: {:?}",
        acks[0].state
    );
    assert_eq!(
        reg.list()[0].unacked,
        1,
        "the sender is still waiting for an answer"
    );
    assert_eq!(reg.follow_up("mute", id).now(), FollowUp::Sent { round: 1 });
    assert!(
        matches!(
            reg.flush_pending().now()[0].items[..],
            [InboxItem::FollowUp {
                delivered: true,
                ..
            }]
        ),
        "the follow-up marks 'read but silent' rather than 'not picked up'"
    );
    // Speaking up answers what it had already read, even though a later run says it.
    assert!(reg.finish("mute", Vec::new(), 1).now().is_none());
    assert_eq!(
        reg.acks_of("mute").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Answered { run: 2 },
        "the answering round adds the receipt"
    );
    assert_eq!(reg.list()[0].unacked, 0);
}

/// The chase also ends when there is nothing left to chase: a stopped instance drops the
/// message, a deleted one takes the record with it. Both are reportable outcomes, not silence.
#[test]
fn follow_up_settles_on_a_dropped_message_and_a_gone_instance() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    let id = reg
        .deliver(
            "w",
            crate::channels::MAIN_NAME,
            "is it too late",
            Vec::new(),
            Some(Duration::from_secs(10)),
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reg.follow_up("w", id).now(), FollowUp::Sent { round: 1 });
    reg.stop("w").now().unwrap_or_else(|e| panic!("{e}"));
    assert!(
        matches!(
            reg.follow_up("w", id).now(),
            FollowUp::Settled(AckState::Dropped { .. })
        ),
        "stopping discards"
    );
    reg.remove("w").now().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reg.follow_up("w", id).now(), FollowUp::Gone);
    assert_eq!(reg.follow_up("ghost", MsgId(999)).now(), FollowUp::Gone);
}

/// A run chain that dies with messages still queued must not strand them: the instance goes
/// back to Idle and the recovery dispatcher picks the batch up.
#[test]
fn messages_survive_a_failed_run_and_are_retried() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    reg.deliver(
        "w",
        crate::channels::MAIN_NAME,
        "continue",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("{e}"));
    // The run failed (spawn_agent_loop's error branch) — it only marks the instance idle.
    reg.mark_idle("w");
    assert_eq!(
        reg.list()[0].pending,
        1,
        "the message is still in the inbox"
    );
    let woken = reg.flush_pending().now();
    assert_eq!(woken.len(), 1, "the recovery dispatcher re-delivers");
    assert_eq!(woken[0].items.len(), 1);
}

#[test]
fn delivered_messages_require_output_after_their_delivery_offset() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    let _ = reg.next_run("w").now();
    let id = reg
        .deliver(
            "w",
            crate::channels::MAIN_NAME,
            "late instruction",
            Vec::new(),
            None,
        )
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    let items = reg.take_running("w", 5).now();
    assert_eq!(items.len(), 1);
    assert_eq!(
        reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Delivered { run: 1 }
    );
    assert!(reg.finish("w", Vec::new(), 5).now().is_none());
    assert_eq!(
        reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Delivered { run: 1 },
        "text produced before delivery does not answer it"
    );
    assert_eq!(reg.follow_up("w", id).now(), FollowUp::Sent { round: 1 });
    let follow_up = reg.flush_pending().now();
    assert_eq!(follow_up.len(), 1);
    assert!(reg.finish("w", Vec::new(), 1).now().is_none());
    assert_eq!(
        reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Answered { run: 2 },
        "later text does answer the previously delivered message"
    );
}

/// A colleague's message is not answered by the receiver having spoken
/// (D137): a peer reads its inbox and nothing else, so turn text — which is
/// exactly what settles main's — settles nothing here. What settles it is a
/// message going back.
#[test]
fn a_peer_is_answered_by_a_message_back_not_by_turn_text() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("qa", AgentKind::Hire, None, "qa".into(), test_session())
        .now();
    reg.insert("dev", AgentKind::Hire, None, "dev".into(), test_session())
        .now();
    let _ = reg.next_run("qa").now();
    reg.deliver("qa", "dev", "does the parser handle EOF?", Vec::new(), None)
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    let _ = reg.take_running("qa", 0).now();
    assert!(matches!(
        reg.acks_of("qa").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Delivered { .. }
    ));

    // A whole turn's worth of prose, and dev has still heard nothing.
    assert!(reg.finish("qa", Vec::new(), 500).now().is_none());
    assert!(
        matches!(
            reg.acks_of("qa").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Delivered { .. }
        ),
        "turn text goes to main; the colleague who asked cannot read it"
    );

    // The message back is the answer.
    reg.deliver("dev", "qa", "it does, since the rewrite", Vec::new(), None)
        .now()
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        matches!(
            reg.acks_of("qa").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Answered { .. }
        ),
        "and it settles the record the chase reads"
    );
}

/// Main's own messages keep the rule they have had since D44 — it reads the
/// turn text, so the turn text answers it.
#[test]
fn mains_message_is_still_answered_by_turn_text() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("qa", AgentKind::Hire, None, "qa".into(), test_session())
        .now();
    let _ = reg.next_run("qa").now();
    reg.deliver(
        "qa",
        crate::channels::MAIN_NAME,
        "status?",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("{e}"));
    let _ = reg.take_running("qa", 0).now();
    assert!(reg.finish("qa", Vec::new(), 500).now().is_none());
    assert!(matches!(
        reg.acks_of("qa").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Answered { .. }
    ));
}

#[test]
fn restored_running_batch_returns_to_queued_state() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .now();
    let _ = reg.next_run("w").now();
    reg.deliver(
        "w",
        crate::channels::MAIN_NAME,
        "retry me",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("{e}"));
    let items = reg.take_running("w", 0).now();
    assert!(matches!(
        reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Delivered { run: 1 }
    ));
    reg.restore_inbox("w", items);
    reg.settle_now();
    assert_eq!(
        reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
        AckState::Queued
    );
    reg.mark_idle("w");
    let retry = reg.flush_pending().now();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].run, 2);
}

#[tokio::test]
async fn stop_wins_before_a_claimed_run_installs_its_abort_handle() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("w", AgentKind::Hire, None, "w".into(), test_session())
        .await;
    reg.mark_idle("w");
    reg.deliver("w", crate::channels::MAIN_NAME, "start", Vec::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let wake = reg
        .flush_pending()
        .await
        .pop()
        .unwrap_or_else(|| unreachable!());
    assert_eq!(wake.run, 1);
    reg.stop("w").await.unwrap_or_else(|e| panic!("{e}"));
    let task = tokio::spawn(async { std::future::pending::<()>().await });
    assert!(
        !reg.set_abort_if_running("w", wake.run, task.abort_handle(), wake.items)
            .await,
        "a stopped entry rejects the late handle"
    );
    assert!(!reg.accepts_run("w", wake.run).await);
}

#[test]
fn stop_and_delete_semantics() {
    let reg = crate::app::AppCore::start(Default::default()).agents();
    reg.insert("x", AgentKind::Hire, None, "x".into(), test_session())
        .now();
    reg.set_run_watch("x", crate::watch::WatchId(7));
    assert_eq!(
        reg.stop("x").now().unwrap_or_else(|e| panic!("{e}")),
        (Some(crate::watch::WatchId(7)), 0),
        "stopping while running returns the current watch line"
    );
    assert!(
        reg.stop("x")
            .now()
            .unwrap_or_else(|e| panic!("{e}"))
            .0
            .is_none(),
        "idempotent"
    );
    // Turn finishing after a stop: history is still archived, no revival.
    assert!(
        reg.finish("x", vec![Message::user_text("h")], 1)
            .now()
            .is_none()
    );
    assert_eq!(reg.list()[0].state, AgentState::Stopped);
    // A direct message after the stop is the one thing that revives (D105a).
    reg.deliver(
        "x",
        crate::channels::MAIN_NAME,
        "still there",
        Vec::new(),
        None,
    )
    .now()
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        reg.list()[0].state,
        AgentState::Idle,
        "resumed, not refused"
    );
    reg.remove("x").now().unwrap_or_else(|e| panic!("{e}"));
    assert!(reg.list().is_empty());
    assert_eq!(reg.claim_name("x").now(), "x", "deletion frees the name");
    assert!(
        reg.deliver("x", crate::channels::MAIN_NAME, "hi", Vec::new(), None)
            .now()
            .is_err(),
        "unknown instance errors"
    );
    // Stopping an idle instance: no active line.
    reg.insert("y", AgentKind::Hire, None, "y".into(), test_session())
        .now();
    reg.set_run_watch("y", crate::watch::WatchId(9));
    assert!(reg.finish("y", Vec::new(), 1).now().is_none());
    assert!(
        reg.stop("y")
            .now()
            .unwrap_or_else(|e| panic!("{e}"))
            .0
            .is_none(),
        "stopping while idle does not cancel a terminal watch line"
    );
}
