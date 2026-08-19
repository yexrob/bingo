//! The actor's own tests: sequencing, the snapshot cut, the reads it answers,
//! and the actions it applies.
//!
//! Split from the loop for the same reason `app_server/stdio` splits its own:
//! what the actor *is* and what it is *asserted to be* are two readings, and one
//! file carrying both had grown past the size at which either could be found.

use super::*;
use crate::app::event::CatalogChanged;
use crate::app::snapshot::{CatalogKind, ThinkingLevel};
use crate::app::{AppCore, AppPublisher, RequestId};

fn catalog(revision: u64) -> AppEventPayload {
    AppEventPayload::CatalogChanged(CatalogChanged {
        catalog: CatalogKind::Models,
        revision,
    })
}

fn revision_of(frame: &AppFrame) -> u64 {
    match frame {
        AppFrame::Event(event) => match &event.payload {
            AppEventPayload::CatalogChanged(changed) => changed.revision,
            other => panic!("expected a catalog event, got {other:?}"),
        },
        other => panic!("expected an event, got {other:?}"),
    }
}

fn publish(publisher: &AppPublisher, revision: u64) {
    publisher
        .publish(catalog(revision), None)
        .unwrap_or_else(|error| panic!("{error}"));
}

/// Attach, then take the cut every attachment starts from.
async fn attached(core: &AppCore, label: &str) -> (AppLink, SessionSnapshot) {
    let mut link = core
        .attach(AttachRequest::new(label))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    link.request(AppRequest::Query {
        id: RequestId(1),
        query: AppQuery::ReadSession,
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match link.recv().await {
        Some(AppFrame::Reply {
            result: Ok(AppReply::Session(snapshot)),
            ..
        }) => (link, *snapshot),
        other => panic!("expected a session snapshot, got {other:?}"),
    }
}

/// The one ordering point has to hold under every producer at once: N agent
/// runs publishing concurrently still make one history, with no number
/// skipped and none repeated.
#[tokio::test]
async fn concurrent_producers_still_make_one_gapless_sequence() {
    const PRODUCERS: u64 = 8;
    const EACH: u64 = 25;
    let core = AppCore::start(SessionSetup::default());
    let (mut link, snapshot) = attached(&core, "test").await;
    assert_eq!(snapshot.event_cursor, 0, "nothing has happened yet");

    let mut runs = Vec::new();
    for producer in 0..PRODUCERS {
        let publisher = core.publisher();
        runs.push(tokio::spawn(async move {
            for step in 0..EACH {
                publish(&publisher, producer * EACH + step);
            }
        }));
    }
    for run in runs {
        run.await.unwrap_or_else(|error| panic!("{error}"));
    }

    let mut seen = Vec::new();
    for _ in 0..(PRODUCERS * EACH) {
        match link.recv().await {
            Some(AppFrame::Event(event)) => seen.push(event.meta.seq),
            other => panic!("expected an event, got {other:?}"),
        }
    }
    assert_eq!(
        seen,
        (1..=PRODUCERS * EACH).collect::<Vec<_>>(),
        "sequence numbers are strictly increasing and gapless, in arrival order"
    );
}

/// The cut is a barrier, not a hint: what happened before it is in the
/// snapshot, so the stream starts strictly after it.
#[tokio::test]
async fn a_snapshot_cut_suppresses_what_it_already_contains() {
    let core = AppCore::start(SessionSetup::default());
    let publisher = core.publisher();
    let mut link = core
        .attach(AttachRequest::new("test"))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    for revision in 0..3 {
        publish(&publisher, revision);
    }

    link.request(AppRequest::Query {
        id: RequestId(7),
        query: AppQuery::ReadSession,
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    let cursor = match link.recv().await {
        Some(AppFrame::Reply {
            id,
            result: Ok(AppReply::Session(snapshot)),
        }) => {
            assert_eq!(id, RequestId(7), "the reply names the request it answers");
            snapshot.event_cursor
        }
        other => panic!("expected the snapshot first, got {other:?}"),
    };
    assert_eq!(cursor, 3, "the cut names the last event it contains");

    publish(&publisher, 99);
    match link.recv().await {
        Some(AppFrame::Event(event)) => {
            assert_eq!(
                event.meta.seq,
                cursor + 1,
                "the first event after a cut is the next one, never a replay"
            );
            assert!(event.meta.ts > 0, "the actor stamps the instant it decided");
        }
        other => panic!("expected the event after the cut, got {other:?}"),
    }
}

/// Two frontends attach at different moments and each reads from its own
/// cut. Neither is told the other's past.
#[tokio::test]
async fn two_attachments_read_from_their_own_cursors() {
    let core = AppCore::start(SessionSetup::default());
    let publisher = core.publisher();
    let (mut early, early_snapshot) = attached(&core, "early").await;
    assert_eq!(early_snapshot.event_cursor, 0);
    publish(&publisher, 1);
    publish(&publisher, 2);

    let (mut late, late_snapshot) = attached(&core, "late").await;
    assert_eq!(late_snapshot.event_cursor, 2, "the second cut is later");
    publish(&publisher, 3);

    let mut seen = Vec::new();
    for _ in 0..3 {
        match early.recv().await {
            Some(frame) => seen.push(revision_of(&frame)),
            None => panic!("the early attachment closed"),
        }
    }
    assert_eq!(seen, vec![1, 2, 3], "the early attachment saw all three");
    match late.recv().await {
        Some(frame) => assert_eq!(
            revision_of(&frame),
            3,
            "the late attachment starts after its own cut"
        ),
        None => panic!("the late attachment closed"),
    }
}

/// Every identifier comes from the actor, inside one epoch.
#[tokio::test]
async fn the_session_is_identified_by_the_epoch_that_minted_it() {
    let core = AppCore::start(SessionSetup {
        title: "Notes".to_string(),
        provider: "default".to_string(),
        model: "sonnet".to_string(),
        ..SessionSetup::default()
    });
    let (_link, snapshot) = attached(&core, "test").await;
    assert!(snapshot.session.id.as_str().starts_with(SessionId::PREFIX));
    assert!(snapshot.session.epoch.as_str().starts_with(EpochId::PREFIX));
    assert_eq!(snapshot.session.title, "Notes");
    assert_eq!(snapshot.config.model, "sonnet");
    assert_eq!(snapshot.session.state, SessionState::Active);
    assert!(snapshot.active_turns.is_empty(), "nothing is running yet");
    match snapshot.conversations.active.as_slice() {
        [main] => {
            assert_eq!(main.kind, crate::app::snapshot::ConversationKind::Main);
            assert_eq!(main.unread, 0);
            assert!(main.obligations.is_empty());
        }
        other => panic!("a session has exactly one conversation to start with: {other:?}"),
    }
}

/// Two methods are left, and they are the two that are not this actor's to
/// answer: one `AppCore` *is* one session, so starting or resuming another
/// replaces it rather than asking it. The transport owns that lifecycle
/// (B6). The reply is still a reply: the request is answered, never dropped.
#[tokio::test]
async fn the_two_methods_that_choose_a_session_belong_to_the_transport() {
    let core = AppCore::start(SessionSetup::default());
    let (mut link, _) = attached(&core, "test").await;
    for (id, command, name) in [
        (
            2,
            AppCommand::StartSession {
                cwd: None,
                provider: None,
                model: None,
                thinking: None,
                permission_mode: None,
            },
            "session/start",
        ),
        (
            3,
            AppCommand::ResumeSession {
                locator: crate::app::snapshot::SessionLocator::Latest,
            },
            "session/resume",
        ),
    ] {
        link.request(AppRequest::Command {
            id: RequestId(id),
            command,
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            next_reply(&mut link, RequestId(id)).await,
            Err(AppError::Unserved(name))
        );
    }
}

/// Reading a conversation never marks it read; only `markRead` does, and it
/// names the revision it believed it was looking at (spec invariant #14).
#[tokio::test]
async fn marking_read_names_the_view_it_was_looking_at() {
    let core = AppCore::start(SessionSetup::default());
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));
    let revision = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.revision)
        .unwrap_or_default();

    link.request(AppRequest::Command {
        id: RequestId(2),
        command: AppCommand::MarkRead {
            conversation_id: main.clone(),
            last_item_id: None,
            last_room_seq: None,
            expected_revision: revision.saturating_add(7),
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(2)).await {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::StaleRevision,
            "a view the client never saw cannot clear attention"
        ),
        other => panic!("expected a stale revision, got {other:?}"),
    }

    link.request(AppRequest::Command {
        id: RequestId(3),
        command: AppCommand::MarkRead {
            conversation_id: main,
            last_item_id: None,
            last_room_seq: None,
            expected_revision: revision,
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    // The reply is the reader's own view, so a client that just cleared a
    // badge does not have to wait for a notification to know it is clear.
    match next_reply(&mut link, RequestId(3)).await {
        Ok(AppReply::Marked(summary)) => {
            assert_eq!(summary.unread, 0);
            assert_eq!(summary.mentions, 0);
        }
        other => panic!("expected the marked view, got {other:?}"),
    }
}

/// A core whose settings and directories are real, so the reads have
/// something to read.
fn configured(tag: &str) -> (AppCore, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!("bingo-reads-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::create_dir_all(home.join("bingo"));
    let _ = std::fs::write(
        home.join("bingo").join("settings.json"),
        r#"{"apiKey": "sk-test", "permissions": {"allow": ["Bash(cargo test:*)"]},
            "mcpServers": {"docs": {"command": "docs-server"}}}"#,
    );
    let settings = crate::settings::load_settings(&home, &home)
        .unwrap_or_else(|error| panic!("settings: {error}"));
    let core = AppCore::start(SessionSetup {
        model: "sonnet".to_string(),
        provider: "default".to_string(),
        catalog: crate::app::catalog::CatalogSource::load(&home, &home, &home, settings),
        ..SessionSetup::default()
    });
    (core, home)
}

async fn read(link: &mut AppLink, id: u64, query: AppQuery) -> Result<AppReply, AppError> {
    link.request(AppRequest::Query {
        id: RequestId(id),
        query,
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    next_reply(link, RequestId(id)).await
}

/// `config/read` answers out of settings: the effective selection, the rules
/// that are in force, and which layer file contributed what.
#[tokio::test]
async fn the_configuration_says_where_it_came_from() {
    let (core, home) = configured("config");
    let (mut link, _) = attached(&core, "test").await;
    match read(&mut link, 2, AppQuery::ReadConfig).await {
        Ok(AppReply::Config(config)) => {
            assert_eq!(config.model, "sonnet");
            assert_eq!(
                config
                    .permissions
                    .iter()
                    .map(|rule| rule.rule.as_str())
                    .collect::<Vec<_>>(),
                vec!["Bash(cargo test:*)"]
            );
            assert!(
                config
                    .layers
                    .iter()
                    .any(|layer| layer.keys.iter().any(|key| key == "permissions")),
                "the layer that carries the rules is named: {:?}",
                config.layers
            );
            assert_eq!(
                config
                    .mcp_servers
                    .iter()
                    .map(|server| (server.name.as_str(), server.status))
                    .collect::<Vec<_>>(),
                vec![("docs", crate::app::snapshot::McpStatus::Disconnected)],
                "configured is not connected"
            );
        }
        other => panic!("expected a configuration, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `catalog/read` is answerable from the same state, and what MCP reports
/// lands in both the catalog and the configuration.
#[tokio::test]
async fn a_catalog_reads_settings_and_takes_what_mcp_reports() {
    let (core, home) = configured("catalog");
    let (mut link, _) = attached(&core, "test").await;
    match read(
        &mut link,
        2,
        AppQuery::ReadCatalog {
            catalog: CatalogKind::Providers,
            provider: None,
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Ok(AppReply::Catalog(catalog)) => match *catalog {
            crate::app::snapshot::Catalog::Providers(page) => assert_eq!(
                page.items.first().map(|info| info.name.as_str()),
                Some("default")
            ),
            other => panic!("expected providers, got {other:?}"),
        },
        other => panic!("expected a catalog, got {other:?}"),
    }

    core.report_mcp(vec![crate::app::snapshot::McpServerState {
        name: "docs".to_string(),
        enabled: true,
        status: crate::app::snapshot::McpStatus::Connected,
        tools: 3,
        error: None,
    }]);
    settle(&core.control).await;
    match read(
        &mut link,
        3,
        AppQuery::ReadCatalog {
            catalog: CatalogKind::McpServers,
            provider: None,
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Ok(AppReply::Catalog(catalog)) => match *catalog {
            crate::app::snapshot::Catalog::McpServers(page) => {
                assert_eq!(
                    page.items[0].status,
                    crate::app::snapshot::McpStatus::Connected
                );
                assert_eq!(page.items[0].tools, 3);
            }
            other => panic!("expected servers, got {other:?}"),
        },
        other => panic!("expected a catalog, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `action/list` publishes the registry, and availability is answered from
/// the state the core is actually in.
#[tokio::test]
async fn the_actions_are_listed_with_what_can_run_now() {
    let (core, home) = configured("actions");
    let (mut link, _) = attached(&core, "test").await;
    match read(
        &mut link,
        2,
        AppQuery::ListActions {
            origin_conversation_id: None,
        },
    )
    .await
    {
        Ok(AppReply::Actions { actions, .. }) => {
            assert_eq!(actions.len(), crate::app::action::ACTIONS.len());
            let theme = actions
                .iter()
                .find(|info| info.id.as_str() == "theme.set")
                .unwrap_or_else(|| panic!("theme.set is published"));
            assert!(theme.available, "a preference needs nothing");
            assert_eq!(
                theme
                    .arguments
                    .first()
                    .map(|argument| argument.choices.as_slice()),
                Some(["dark".to_string(), "light".to_string(), "auto".to_string()].as_slice()),
                "the argument schema comes from the same table"
            );
            let compact = actions
                .iter()
                .find(|info| info.id.as_str() == "conversation.compact")
                .unwrap_or_else(|| panic!("compact is published"));
            assert!(
                !compact.available && compact.unavailable_reason.is_some(),
                "an action that needs an engine says so rather than failing later"
            );
        }
        other => panic!("expected actions, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `resource/read` pages the same collections a session snapshot carries a
/// bounded head of, and `queue/read` reads one conversation's queue.
#[tokio::test]
async fn the_runtime_collections_and_the_queue_are_paged() {
    let (core, home) = configured("resources");
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));
    match read(
        &mut link,
        2,
        AppQuery::ReadResource {
            resource: crate::app::snapshot::ResourceKind::Rooms,
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Ok(AppReply::Resource(page)) => match *page {
            crate::app::snapshot::ResourcePage::Rooms(rooms) => {
                assert!(rooms.items.is_empty(), "no rooms yet, and that is a page");
                assert_eq!(rooms.next_cursor, None);
            }
            other => panic!("expected rooms, got {other:?}"),
        },
        other => panic!("expected a resource page, got {other:?}"),
    }
    match read(
        &mut link,
        3,
        AppQuery::ReadQueue {
            conversation_id: main,
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Ok(AppReply::Queue { entries, count }) => {
            assert_eq!(count, 0);
            assert!(entries.items.is_empty());
        }
        other => panic!("expected a queue, got {other:?}"),
    }
    match read(
        &mut link,
        4,
        AppQuery::ReadQueue {
            conversation_id: crate::app::ids::ConversationId::new("conv_nope"),
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `asset/registerPath` then `asset/readChunk`: the bytes go into the
/// server's own storage, the caller's file is no longer needed, and the
/// image shows up in the catalog that lists them.
#[tokio::test]
async fn an_asset_is_registered_and_read_back_through_the_core() {
    let (core, home) = configured("assets");
    let (mut link, _) = attached(&core, "test").await;
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(4, 2, image::Rgba([9, 9, 9, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap_or_else(|error| panic!("{error}"));
    let source = home.join("shot.png");
    std::fs::write(&source, &png).unwrap_or_else(|error| panic!("{error}"));

    link.request(AppRequest::Command {
        id: RequestId(2),
        command: AppCommand::RegisterAsset {
            path: source.clone(),
            expected_mime: Some("image/png".to_string()),
            expected_sha256: None,
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    let record = match next_reply(&mut link, RequestId(2)).await {
        Ok(AppReply::Asset(record)) => *record,
        other => panic!("expected an asset, got {other:?}"),
    };
    assert_eq!(record.bytes, png.len() as u64);
    assert_eq!((record.width, record.height), (Some(4), Some(2)));
    // The caller's file is the server's business no longer.
    std::fs::remove_file(&source).unwrap_or_else(|error| panic!("{error}"));

    let mut back = Vec::new();
    let mut offset = 0;
    let mut id = 3;
    loop {
        let reply = read(
            &mut link,
            id,
            AppQuery::ReadAssetChunk {
                asset_id: record.id.clone(),
                offset,
                length: 32,
            },
        )
        .await;
        let (data, next, eof) = match reply {
            Ok(AppReply::AssetChunk {
                data,
                next_offset,
                eof,
            }) => (data, next_offset, eof),
            other => panic!("expected a chunk, got {other:?}"),
        };
        use base64::Engine;
        back.extend(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap_or_else(|error| panic!("{error}")),
        );
        offset = next;
        id += 1;
        if eof {
            break;
        }
    }
    assert_eq!(back, png, "byte for byte, through the request path");

    match read(
        &mut link,
        id,
        AppQuery::ReadCatalog {
            catalog: CatalogKind::Images,
            provider: None,
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Ok(AppReply::Catalog(catalog)) => match *catalog {
            crate::app::snapshot::Catalog::Images(page) => {
                assert_eq!(
                    page.items
                        .iter()
                        .map(|image| image.asset_id.clone())
                        .collect::<Vec<_>>(),
                    vec![record.id.clone()]
                );
                assert_eq!(page.items[0].label.as_deref(), Some("shot.png"));
            }
            other => panic!("expected images, got {other:?}"),
        },
        other => panic!("expected a catalog, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

async fn execute(
    link: &mut AppLink,
    id: u64,
    origin: &ConversationId,
    action: crate::app::command::Action,
) -> Result<AppReply, AppError> {
    link.request(AppRequest::Command {
        id: RequestId(id),
        command: AppCommand::Execute {
            origin_conversation_id: origin.clone(),
            precondition: None,
            action,
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    next_reply(link, RequestId(id)).await
}

fn applied(reply: Result<AppReply, AppError>) -> crate::app::command::ActionResultStatus {
    use crate::app::command::SubmitDisposition;
    match reply {
        Ok(AppReply::Submitted(SubmitDisposition::Applied { result })) => result.status,
        other => panic!("expected an applied action, got {other:?}"),
    }
}

/// Every action the core owns outright, executed through `action/execute`:
/// it changes what the core publishes, it writes settings where they take
/// effect, and asking twice says nothing changed the second time.
#[tokio::test]
async fn the_actions_the_core_owns_are_executed_and_persisted() {
    use crate::app::command::{Action, ActionResultStatus};
    use crate::app::snapshot::{PermissionMode, PermissionRuleDecision, ThemeChoice};
    let (core, home) = configured("execute");
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));

    assert_eq!(
        applied(
            execute(
                &mut link,
                2,
                &main,
                Action::ThemeSet {
                    theme: ThemeChoice::Dark
                }
            )
            .await
        ),
        ActionResultStatus::Applied
    );
    assert_eq!(
        applied(
            execute(
                &mut link,
                3,
                &main,
                Action::ThemeSet {
                    theme: ThemeChoice::Dark
                }
            )
            .await
        ),
        ActionResultStatus::NoChange,
        "choosing what is already chosen changes nothing"
    );
    assert_eq!(
        applied(
            execute(
                &mut link,
                4,
                &main,
                Action::ThinkingSelect {
                    level: ThinkingLevel::High
                }
            )
            .await
        ),
        ActionResultStatus::Applied
    );
    assert_eq!(
        applied(
            execute(
                &mut link,
                5,
                &main,
                Action::PermissionModeSet {
                    mode: PermissionMode::Plan
                }
            )
            .await
        ),
        ActionResultStatus::Applied
    );
    assert_eq!(
        applied(
            execute(
                &mut link,
                6,
                &main,
                Action::ModelSelect {
                    model: "opus".to_string()
                }
            )
            .await
        ),
        ActionResultStatus::Applied
    );
    assert_eq!(
        applied(
            execute(
                &mut link,
                7,
                &main,
                Action::PermissionRuleAdd {
                    decision: PermissionRuleDecision::Deny,
                    rule: "Bash(rm:*)".to_string(),
                }
            )
            .await
        ),
        ActionResultStatus::Applied
    );
    assert_eq!(
        applied(
            execute(
                &mut link,
                8,
                &main,
                Action::McpDisable {
                    server: "docs".to_string()
                }
            )
            .await
        ),
        ActionResultStatus::Applied
    );
    assert_eq!(
        applied(execute(&mut link, 9, &main, Action::SessionGarbageCollect).await),
        ActionResultStatus::NoChange,
        "there is nothing expired to clean"
    );

    match read(&mut link, 10, AppQuery::ReadConfig).await {
        Ok(AppReply::Config(config)) => {
            assert_eq!(config.theme, ThemeChoice::Dark);
            assert_eq!(config.thinking, ThinkingLevel::High);
            assert_eq!(config.permission_mode, PermissionMode::Plan);
            assert_eq!(config.model, "opus");
            assert!(
                config
                    .permissions
                    .iter()
                    .any(|rule| rule.rule == "Bash(rm:*)"
                        && rule.decision == PermissionRuleDecision::Deny)
            );
            assert_eq!(
                config
                    .mcp_servers
                    .iter()
                    .map(|server| server.enabled)
                    .collect::<Vec<_>>(),
                vec![false],
                "the server was turned off in settings, and the read says so"
            );
        }
        other => panic!("expected a configuration, got {other:?}"),
    }
    let written = std::fs::read_to_string(home.join("bingo").join("settings.json"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        written.contains("\"theme\"") && written.contains("dark"),
        "the choice was written where it takes effect: {written}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// An action that needs a model, a transcript rewrite or a network round
/// trip is refused by the same rule `action/list` publishes, rather than
/// letting the client find out by failing halfway.
#[tokio::test]
async fn an_action_that_needs_an_engine_is_refused_before_it_starts() {
    use crate::app::command::Action;
    let (core, home) = configured("engine");
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));
    for (id, action) in [
        (2, Action::ConversationCompact { instructions: None }),
        (3, Action::TeamStart { members: None }),
        (
            4,
            Action::SkillInvoke {
                skill: "guide".to_string(),
                input: None,
            },
        ),
    ] {
        match execute(&mut link, id, &main, action).await {
            Err(AppError::Refused(kind)) => assert_eq!(
                kind,
                crate::app_server::protocol::error::ProtocolErrorKind::ActionUnavailable
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    match execute(
        &mut link,
        5,
        &crate::app::ids::ConversationId::new("conv_nope"),
        Action::SessionGarbageCollect,
    )
    .await
    {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound,
            "an action carries the page it was submitted on"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// A stale precondition fails rather than overwriting a view somebody else
/// refreshed.
#[tokio::test]
async fn a_stale_precondition_loses_rather_than_overwrites() {
    use crate::app::command::Action;
    use crate::app::snapshot::{ResourceRevision, RevisionScope, ThemeChoice};
    let (core, home) = configured("stale");
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));
    link.request(AppRequest::Command {
        id: RequestId(2),
        command: AppCommand::Execute {
            origin_conversation_id: main,
            precondition: Some(ResourceRevision {
                scope: RevisionScope::Config,
                revision: 999,
            }),
            action: Action::ThemeSet {
                theme: ThemeChoice::Light,
            },
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(2)).await {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::StaleRevision
        ),
        other => panic!("expected a stale revision, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `turn/interrupt` asks a running turn to stop, is idempotent, and cannot
/// reach a turn this epoch never minted.
#[tokio::test]
async fn interrupting_asks_the_turn_that_was_named() {
    let (core, home) = configured("interrupt");
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));
    let turns = core.turns();
    let turn = turns
        .open(
            ConvKey::Main,
            crate::app::snapshot::TurnOrigin::User,
            Vec::new(),
        )
        .now()
        .unwrap_or_else(|| panic!("a turn opens"));

    link.request(AppRequest::Command {
        id: RequestId(2),
        command: AppCommand::Interrupt {
            conversation_id: main.clone(),
            turn_id: turn.clone(),
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(2)).await {
        Ok(AppReply::Interrupted { accepted, .. }) => assert!(accepted),
        other => panic!("expected an interrupt, got {other:?}"),
    }
    assert!(
        turns.view().is_interrupted(&turn),
        "the run watches this to know it was asked to stop"
    );

    turns.close(
        turn.clone(),
        crate::app::snapshot::TurnStatus::Interrupted,
        None,
    );
    settle(&core.control).await;
    link.request(AppRequest::Command {
        id: RequestId(3),
        command: AppCommand::Interrupt {
            conversation_id: main.clone(),
            turn_id: turn,
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(3)).await {
        Ok(AppReply::Interrupted { accepted, .. }) => assert!(
            !accepted,
            "a turn that already ended is not interrupted again"
        ),
        other => panic!("expected an interrupt, got {other:?}"),
    }
    link.request(AppRequest::Command {
        id: RequestId(4),
        command: AppCommand::Interrupt {
            conversation_id: main,
            turn_id: crate::app::ids::TurnId::new("turn_nope"),
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(4)).await {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::TurnClosed
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `session/delete` drops a session that is not the open one, and refuses to
/// pull the floor out from under this one.
#[tokio::test]
async fn a_session_that_is_not_this_one_can_be_deleted() {
    use crate::app::snapshot::SessionLocator;
    let (core, home) = configured("delete");
    let transcript = crate::transcript::create(&home, &home)
        .unwrap_or_else(|error| panic!("transcript: {error}"));
    let _ = transcript.append(&crate::api::types::Message::user_text("hi"));
    let path = transcript.path().to_path_buf();
    let (mut link, _) = attached(&core, "test").await;
    link.request(AppRequest::Command {
        id: RequestId(2),
        command: AppCommand::DeleteSession {
            locator: SessionLocator::Stem {
                stem: transcript.name(),
            },
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(2)).await {
        Ok(AppReply::Deleted { deleted, .. }) => assert!(deleted),
        other => panic!("expected a deletion, got {other:?}"),
    }
    assert!(!path.exists(), "the transcript is gone");
    link.request(AppRequest::Command {
        id: RequestId(3),
        command: AppCommand::DeleteSession {
            locator: SessionLocator::Latest,
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(3)).await {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::BadArgument,
            "\"the latest\" is not a name for something to delete"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// A slash line submitted through the composer is the same action a typed
/// call makes, read by the same table.
#[tokio::test]
async fn a_slash_line_is_the_action_a_typed_call_would_have_made() {
    use crate::app::command::{ComposerMode, Submission};
    use crate::app::snapshot::ThemeChoice;
    let (core, home) = configured("composer");
    let (mut link, snapshot) = attached(&core, "test").await;
    let main = snapshot
        .conversations
        .active
        .first()
        .map(|summary| summary.id.clone())
        .unwrap_or_else(|| panic!("main exists"));
    let submit = |text: &str| Submission::Composer {
        mode: ComposerMode::Normal,
        text: text.to_string(),
        attachments: Vec::new(),
    };
    link.request(AppRequest::Command {
        id: RequestId(2),
        command: AppCommand::Submit {
            conversation_id: main.clone(),
            input: submit("/theme light"),
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        applied(next_reply(&mut link, RequestId(2)).await),
        crate::app::command::ActionResultStatus::Applied
    );
    match read(&mut link, 3, AppQuery::ReadConfig).await {
        Ok(AppReply::Config(config)) => assert_eq!(config.theme, ThemeChoice::Light),
        other => panic!("expected a configuration, got {other:?}"),
    }

    // A view changes nothing, and its text is each frontend's own.
    link.request(AppRequest::Command {
        id: RequestId(4),
        command: AppCommand::Submit {
            conversation_id: main.clone(),
            input: submit("/status"),
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        applied(next_reply(&mut link, RequestId(4)).await),
        crate::app::command::ActionResultStatus::NoChange
    );

    link.request(AppRequest::Command {
        id: RequestId(5),
        command: AppCommand::Submit {
            conversation_id: main,
            input: submit("/nonesuch"),
        },
    })
    .await
    .unwrap_or_else(|error| panic!("{error}"));
    match next_reply(&mut link, RequestId(5)).await {
        Err(AppError::Refused(kind)) => assert_eq!(
            kind,
            crate::app_server::protocol::error::ProtocolErrorKind::ActionUnavailable
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// `session/list` reads the transcripts on disk and marks the open one.
#[tokio::test]
async fn the_sessions_on_disk_are_listed() {
    let (core, home) = configured("sessions");
    let transcript = crate::transcript::create(&home, &home)
        .unwrap_or_else(|error| panic!("transcript: {error}"));
    let _ = transcript.append(&crate::api::types::Message::user_text("hi"));
    let (mut link, _) = attached(&core, "test").await;
    match read(
        &mut link,
        2,
        AppQuery::ListSessions {
            cursor: None,
            limit: None,
        },
    )
    .await
    {
        Ok(AppReply::Sessions(page)) => {
            assert_eq!(
                page.items
                    .iter()
                    .map(|entry| entry.title.as_str())
                    .collect::<Vec<_>>(),
                vec![transcript.name().as_str()]
            );
            assert!(page.items[0].message_count >= 1);
        }
        other => panic!("expected sessions, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// The reply to `id`, skipping whatever the core said on the way.
async fn next_reply(link: &mut AppLink, id: RequestId) -> Result<AppReply, AppError> {
    loop {
        match link.recv().await {
            Some(AppFrame::Reply { id: seen, result }) if seen == id => return result,
            Some(_) => {}
            None => panic!("the core closed"),
        }
    }
}

/// A frontend that stops reading loses its attachment rather than stalling
/// the core. It sees the frames it was already handed, then the end.
#[tokio::test]
async fn a_frontend_that_stops_reading_loses_its_attachment() {
    let core = AppCore::start(SessionSetup::default());
    let publisher = core.publisher();
    let (mut link, _) = attached(&core, "silent").await;
    // One frame channel over, and then some: enqueueing never blocks, so
    // every one of these is accepted and the actor writes them out until the
    // silent frontend's channel is full.
    let published = (FRAME_CAPACITY + 8) as u64;
    for revision in 0..published {
        publish(&publisher, revision);
    }
    // The barrier is what makes the overflow a fact rather than a race with
    // the reader below: by the time it answers, every publish above has been
    // written or dropped.
    settle(&core.control).await;
    let mut delivered = 0;
    while let Some(frame) = link.recv().await {
        assert_eq!(revision_of(&frame), delivered);
        delivered += 1;
    }
    assert_eq!(
        delivered, FRAME_CAPACITY as u64,
        "what fit was delivered; the rest closed the attachment instead of blocking the core"
    );
}
