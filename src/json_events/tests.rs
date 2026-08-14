use super::*;

fn turn_start(prompt: String) -> String {
    serde_json::json!({
        "protocolVersion": 1,
        "type": "turn.start",
        "commandId": "command-1",
        "turnId": "turn-1",
        "prompt": prompt,
    })
    .to_string()
}

#[test]
fn command_round_trip_uses_protocol_v1_shape() {
    let command = parse_command_line(turn_start("hello".to_string()).as_bytes())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        command,
        ClientCommand::TurnStart {
            protocol_version: 1,
            command_id: "command-1".to_string(),
            turn_id: "turn-1".to_string(),
            prompt: "hello".to_string(),
        }
    );
    let value = serde_json::to_value(command).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(value["type"], "turn.start");
    assert_eq!(value["protocolVersion"], 1);
}

#[test]
fn every_command_variant_round_trips() {
    let values = [
        serde_json::json!({
            "protocolVersion": 1,
            "type": "turn.cancel",
            "commandId": "command-1",
            "turnId": "turn-1"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "attachment.add",
            "commandId": "command-attachment",
            "attachmentId": "attachment-1",
            "data": "aGVsbG8="
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "prompt.respond",
            "commandId": "command-2",
            "promptId": "prompt-1",
            "response": {"kind": "option", "optionId": "allow"}
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "models.list",
            "commandId": "command-3",
            "provider": "default"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "providers.list",
            "commandId": "command-providers"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "settings.get",
            "commandId": "command-settings"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "context.subscribe",
            "commandId": "command-context"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.subscribe",
            "commandId": "command-team-subscribe"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.refresh",
            "commandId": "command-team-refresh"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.validate",
            "commandId": "command-team-validate"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.save",
            "commandId": "command-team-save",
            "baseRevision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "definition": {
                "schemaVersion": 1,
                "name": "reviewers",
                "members": [{"name": "reviewer", "agent": "reviewer"}]
            }
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.start",
            "commandId": "command-team-start"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.stop",
            "commandId": "command-team-stop"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.avatar.get",
            "commandId": "command-team-avatar-get",
            "avatar": "project:0123456789abcdef01234567"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.list",
            "commandId": "command-task-list"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.get",
            "commandId": "command-task-get",
            "taskId": "task-1",
            "beforeSeq": 42,
            "limit": 100
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.create",
            "commandId": "command-task-create",
            "title": "Review release",
            "description": "Inspect the release blockers",
            "participants": ["reviewer", "tester"],
            "leader": "reviewer"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.post",
            "commandId": "command-task-post",
            "taskId": "task-1",
            "text": "Please verify the fix"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.pause",
            "commandId": "command-task-pause",
            "taskId": "task-1"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.resume",
            "commandId": "command-task-resume",
            "taskId": "task-1",
            "message": "Continue after addressing the review"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.complete",
            "commandId": "command-task-complete",
            "taskId": "task-1"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.task.cancel",
            "commandId": "command-task-cancel",
            "taskId": "task-1"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.message",
            "commandId": "command-agent-message",
            "member": "reviewer",
            "message": "review this"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.stop",
            "commandId": "command-agent-stop",
            "member": "reviewer"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.remove",
            "commandId": "command-agent-remove",
            "member": "reviewer"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.activity.get",
            "commandId": "command-agent-activity",
            "member": "reviewer"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.definition.list",
            "commandId": "command-definition-list"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.definition.get",
            "commandId": "command-definition-get",
            "scope": "project",
            "id": "reviewer"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.definition.save",
            "commandId": "command-definition-save",
            "scope": "project",
            "id": "reviewer",
            "baseRevision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "definition": {
                "name": "Reviewer",
                "description": "Reviews changes",
                "model": "model-1",
                "provider": "default",
                "thinking": "high",
                "inheritSystem": true,
                "system": "Review carefully"
            }
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "agent.definition.archive",
            "commandId": "command-definition-archive",
            "scope": "project",
            "id": "reviewer",
            "baseRevision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "channel.post",
            "commandId": "command-channel-post",
            "channel": "team",
            "text": "hello"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "channel.history.get",
            "commandId": "command-channel-history",
            "channel": "team"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "session.rename",
            "commandId": "command-4",
            "name": "Renamed"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "session.delete",
            "commandId": "command-5"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "session.fork",
            "commandId": "command-fork",
            "reason": "edit-last-prompt",
            "sourceTurnId": "turn-1",
            "sourceRevision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }),
        serde_json::json!({
            "protocolVersion": 1,
            "type": "session.close",
            "commandId": "command-6"
        }),
    ];
    for value in values {
        let line = value.to_string();
        let command =
            parse_command_line(line.as_bytes()).unwrap_or_else(|error| panic!("{value}: {error}"));
        assert_eq!(
            serde_json::to_value(command).unwrap_or_else(|error| panic!("{error}")),
            value
        );
    }
}

#[test]
fn team_v2_starter_blueprint_save_command_is_accepted() {
    let command = serde_json::json!({
        "protocolVersion": 1,
        "type": "team.save",
        "commandId": "command-team-v2-save",
        "baseRevision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "definition": {
            "schemaVersion": 2,
            "teamId": "team-project-team",
            "name": "project-team",
            "leader": "lead",
            "channel": {"mode": "serial", "messageLimit": 500},
            "members": [
                {
                    "memberId": "member-lead-1",
                    "name": "lead",
                    "agent": "team-lead",
                    "profile": {
                        "identity": {"title": "Technical lead"},
                        "personality": "Calm and decisive",
                        "communication": {
                            "language": "auto",
                            "tone": "professional",
                            "verbosity": "balanced"
                        },
                        "constraints": [],
                        "preferences": ["Clarify acceptance criteria"]
                    }
                },
                {
                    "memberId": "member-implementer-2",
                    "name": "implementer",
                    "agent": "team-implementer",
                    "profile": {"constraints": [], "preferences": []}
                },
                {
                    "memberId": "member-reviewer-3",
                    "name": "reviewer",
                    "agent": "team-reviewer",
                    "profile": {"constraints": [], "preferences": []}
                }
            ]
        }
    });

    assert!(parse_command_line(command.to_string().as_bytes()).is_ok());

    let mut unsupported = command;
    unsupported["definition"]["schemaVersion"] = serde_json::Value::from(3);
    let error = parse_command_line(unsupported.to_string().as_bytes())
        .expect_err("unsupported team schema must fail");
    assert!(error.to_string().contains("schemaVersion must be 1 or 2"));
}

#[test]
fn team_avatar_get_is_capability_gated_and_rejects_unsafe_ids() {
    assert!(CAPABILITIES.contains(&"team.avatar.read.v1"));
    let command = |avatar: &str| {
        serde_json::json!({
            "protocolVersion": 1,
            "type": "team.avatar.get",
            "commandId": "command-team-avatar-get",
            "avatar": avatar
        })
        .to_string()
    };
    assert!(parse_command_line(command("project:0123456789abcdef01234567").as_bytes()).is_ok());
    for avatar in [
        "sora",
        "project:../../outside",
        "project:0123456789abcdef0123456",
        "project:0123456789abcdef0123456g",
    ] {
        assert!(
            parse_command_line(command(avatar).as_bytes()).is_err(),
            "unsafe avatar id was accepted: {avatar}"
        );
    }
}

#[test]
fn prompt_and_rename_bounds_count_unicode_scalars() {
    assert!(parse_command_line(turn_start("x".repeat(MAX_PROMPT_CHARS)).as_bytes()).is_ok());
    assert!(parse_command_line(turn_start("x".repeat(MAX_PROMPT_CHARS + 1)).as_bytes()).is_err());
    assert!(parse_command_line(turn_start(" \n\t ".to_string()).as_bytes()).is_err());

    let response = |text: String| {
        serde_json::json!({
            "protocolVersion": 1,
            "type": "prompt.respond",
            "commandId": "command-1",
            "promptId": "prompt-1",
            "response": {"kind": "text", "text": text}
        })
        .to_string()
    };
    assert!(parse_command_line(response("界".repeat(MAX_RESPONSE_CHARS)).as_bytes()).is_ok());
    assert!(parse_command_line(response("界".repeat(MAX_RESPONSE_CHARS + 1)).as_bytes()).is_err());

    let rename = |name: String| {
        serde_json::json!({
            "protocolVersion": 1,
            "type": "session.rename",
            "commandId": "command-1",
            "name": name,
        })
        .to_string()
    };
    assert!(parse_command_line(rename("界".repeat(MAX_RENAME_CHARS)).as_bytes()).is_ok());
    assert!(parse_command_line(rename("界".repeat(MAX_RENAME_CHARS + 1)).as_bytes()).is_err());
}

#[test]
fn invalid_prompt_response_is_rejected_before_the_live_prompt_is_consumed() {
    let (reply, _receiver) = oneshot::channel();
    let prompt = PendingPrompt {
        turn_id: "turn-1".to_string(),
        prompt_id: "prompt-1".to_string(),
        kind: PromptKind::Permission,
        title: "Allow Bash".to_string(),
        question: "Run the command?".to_string(),
        options: vec![
            PromptOption {
                id: "allow".to_string(),
                label: "Allow".to_string(),
                description: None,
            },
            PromptOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
                description: None,
            },
        ],
        allow_free_text: false,
        reply: PromptReply::Permission(reply),
    };

    assert!(!prompt_response_matches(
        &prompt,
        &PromptResponse::Option {
            option_id: "unknown".to_string(),
        }
    ));
    assert!(prompt_response_matches(
        &prompt,
        &PromptResponse::Option {
            option_id: "allow".to_string(),
        }
    ));
}

#[test]
fn command_line_cap_is_enforced_before_parsing() {
    let line = vec![b'x'; MAX_COMMAND_LINE_BYTES + 1];
    let error = parse_command_line(&line).expect_err("oversized line must fail");
    assert!(error.to_string().contains("byte limit"));
}

#[test]
fn attachment_command_validates_identifier_and_encoded_size() {
    let valid = serde_json::json!({
        "protocolVersion": 1,
        "type": "attachment.add",
        "commandId": "command-1",
        "attachmentId": "attachment-1",
        "data": "aGVsbG8="
    });
    assert!(parse_command_line(valid.to_string().as_bytes()).is_ok());

    let missing_id = serde_json::json!({
        "protocolVersion": 1,
        "type": "attachment.add",
        "commandId": "command-1",
        "attachmentId": "",
        "data": "aGVsbG8="
    });
    assert!(parse_command_line(missing_id.to_string().as_bytes()).is_err());

    let oversized_data = serde_json::json!({
        "protocolVersion": 1,
        "type": "attachment.add",
        "commandId": "command-1",
        "attachmentId": "attachment-1",
        "data": "a".repeat(crate::api::image::MAX_DECODE_BYTES.div_ceil(3) * 4 + 1)
    });
    assert!(parse_command_line(oversized_data.to_string().as_bytes()).is_err());
}

#[test]
fn session_fork_commands_enforce_reason_specific_fields() {
    assert!(CAPABILITIES.contains(&"session.fork.v1"));
    let edit = |turn: Option<&str>, revision: Option<&str>| {
        serde_json::json!({
            "protocolVersion": 1,
            "type": "session.fork",
            "commandId": "command-fork-edit",
            "reason": "edit-last-prompt",
            "sourceTurnId": turn,
            "sourceRevision": revision,
        })
        .to_string()
    };
    assert!(parse_command_line(edit(None, None).as_bytes()).is_ok());
    assert!(
        parse_command_line(
            edit(
                Some("turn-1"),
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            )
            .as_bytes()
        )
        .is_ok()
    );
    assert!(parse_command_line(edit(Some("turn-1"), None).as_bytes()).is_err());
    assert!(parse_command_line(edit(None, Some("bad")).as_bytes()).is_err());

    let recovery_with_turn = serde_json::json!({
        "protocolVersion": 1,
        "type": "session.fork",
        "commandId": "command-fork-recovery",
        "reason": "recover-interrupted",
        "sourceTurnId": "turn-1",
    });
    assert!(parse_command_line(recovery_with_turn.to_string().as_bytes()).is_err());
}

#[test]
fn attachment_registration_prepares_and_returns_a_marker() {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 60, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap_or_else(|error| panic!("{error}"));
    let prepared = crate::api::image::prepare_image(&bytes)
        .unwrap_or_else(|| panic!("test image must decode"));
    let attachments = crate::api::image::Attachments::new();
    let id = attachments.register_prepared(prepared.clone());
    assert_eq!(crate::api::image::marker(id), "#[image 1]");
    let resolved = attachments.resolve("inspect #[image 1]");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].media_type, prepared.media_type);
    assert_eq!(resolved[0].data, prepared.data);
}

#[test]
fn version_and_unknown_commands_are_rejected() {
    let wrong_version = serde_json::json!({
        "protocolVersion": 2,
        "type": "session.close",
        "commandId": "command-1"
    });
    assert!(parse_command_line(wrong_version.to_string().as_bytes()).is_err());
    let unknown = serde_json::json!({
        "protocolVersion": 1,
        "type": "unknown",
        "commandId": "command-1"
    });
    assert!(parse_command_line(unknown.to_string().as_bytes()).is_err());
}

#[test]
fn event_writer_assigns_gapless_sequence_and_ndjson_lines() {
    let mut output = Vec::new();
    {
        let mut writer = EventWriter::new(&mut output);
        writer.set_session_id(Some("session-1".to_string()));
        writer
            .emit(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: None,
                code: None,
                msg: "one".to_string(),
            })
            .unwrap_or_else(|error| panic!("{error}"));
        writer
            .emit(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: None,
                code: None,
                msg: "two".to_string(),
            })
            .unwrap_or_else(|error| panic!("{error}"));
    }
    let lines: Vec<serde_json::Value> = std::str::from_utf8(&output)
        .unwrap_or_else(|error| panic!("{error}"))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}")))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["seq"], 1);
    assert_eq!(lines[1]["seq"], 2);
    assert_eq!(lines[0]["sessionId"], "session-1");
    assert!(output.ends_with(b"\n"));
}

#[test]
fn context_hooks_emit_only_after_subscription() {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let subscribed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hooks = json_hooks(
        sender,
        "turn-1".to_string(),
        Arc::new(std::sync::atomic::AtomicU64::new(1)),
        subscribed.clone(),
    );

    (hooks.on_context_usage)(12, 128_000);
    assert!(receiver.try_recv().is_err());

    subscribed.store(true, std::sync::atomic::Ordering::Release);
    (hooks.on_context_usage)(34, 128_000);
    assert!(matches!(
        receiver.try_recv(),
        Ok(AdapterEvent::ContextUsage {
            turn_id,
            used_tokens: 34,
            context_window: 128_000,
        }) if turn_id == "turn-1"
    ));
}

#[test]
fn context_usage_event_serializes_subscription_and_turn_shapes() {
    let mut output = Vec::new();
    {
        let mut writer = EventWriter::new(&mut output);
        writer.set_session_id(Some("session-1".to_string()));
        writer
            .emit(CliEvent::ContextUsage {
                base: EventBase::default(),
                command_id: Some("command-context".to_string()),
                turn_id: None,
                used_tokens: 42,
                context_window: 128_000,
            })
            .unwrap();
        writer
            .emit(CliEvent::ContextUsage {
                base: EventBase::default(),
                command_id: None,
                turn_id: Some("turn-1".to_string()),
                used_tokens: 84,
                context_window: 128_000,
            })
            .unwrap();
    }
    let events = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events[0]["type"], "context.usage");
    assert_eq!(events[0]["commandId"], "command-context");
    assert!(events[0].get("turnId").is_none());
    assert_eq!(events[1]["turnId"], "turn-1");
    assert!(events[1].get("commandId").is_none());
}

#[test]
fn settings_snapshot_never_serializes_credentials() {
    let settings = crate::settings::Settings {
        api_key: Some("top-secret".to_string()),
        ..Default::default()
    };
    let client = crate::api::client::Client::new(
        "top-secret".to_string(),
        "https://example.test".to_string(),
    );
    let encoded = serde_json::to_string(&sanitized_settings(&settings, &client))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(!encoded.contains("top-secret"), "{encoded}");
    assert!(
        encoded.contains("\"credentialConfigured\":true"),
        "{encoded}"
    );
}

#[test]
fn team_merge_preserves_unknown_root_channel_and_member_fields() {
    let existing = serde_json::json!({
        "schemaVersion": 1,
        "name": "old",
        "futureRoot": {"keep": true},
        "channel": {"mode": "serial", "futureChannel": 7},
        "members": [{"name": "reviewer", "agent": "old", "futureMember": "keep"}]
    });
    let incoming = serde_json::json!({
        "schemaVersion": 1,
        "name": "new",
        "channel": {"mode": "free"},
        "members": [{"name": "reviewer", "agent": "reviewer"}]
    });
    let merged = merge_team_value(existing, incoming);
    assert_eq!(merged["futureRoot"]["keep"], true);
    assert_eq!(merged["channel"]["futureChannel"], 7);
    assert_eq!(merged["members"][0]["futureMember"], "keep");
    assert_eq!(merged["name"], "new");
}

#[test]
fn exact_session_resolution_rejects_fragments_and_paths() {
    let root = std::env::temp_dir().join(format!("bingo-json-session-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let cwd = root.join("project");
    std::fs::create_dir_all(&cwd).unwrap_or_else(|error| panic!("{error}"));
    let transcript =
        crate::transcript::create(&home, &cwd).unwrap_or_else(|error| panic!("{error}"));
    transcript
        .append(&Message::user_text("hello"))
        .unwrap_or_else(|error| panic!("{error}"));
    let stem = transcript.name();
    assert_eq!(
        resolve_session(&home, &stem)
            .unwrap_or_else(|error| panic!("{error}"))
            .path(),
        transcript.path()
    );
    assert!(resolve_session(&home, &stem[..stem.len() - 1]).is_err());
    assert!(resolve_session(&home, "../session").is_err());
    let _ = std::fs::remove_dir_all(&root);
}
