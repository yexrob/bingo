use std::io::Write;

use super::*;

impl<W: Write> JsonSession<W> {
    pub(super) fn team_snapshot(&self) -> Result<TeamSnapshot, String> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let path = project_dir.join(crate::team::TEAM_FILE);
        let raw = match std::fs::read(&path) {
            Ok(raw) => Some(raw),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
        };
        let revision = crate::update::sha256_hex(raw.as_deref().unwrap_or_default());
        let mut definition = raw
            .as_deref()
            .and_then(|value| serde_json::from_slice::<serde_json::Value>(value).ok());
        if let Some(serde_json::Value::Object(object)) = definition.as_mut() {
            object
                .entry("schemaVersion".to_string())
                .or_insert_with(|| serde_json::Value::from(1));
        }

        let defs = crate::agents::load_agent_defs(&self.session.home, &project_dir);
        let (tree, validation) = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => {
                let validation =
                    crate::team::validate_tree(&tree, &self.session, &self.session.home)
                        .err()
                        .map(|error| error.to_string());
                (Some(tree), validation)
            }
            Ok(None) => (None, None),
            Err(error) => (None, Some(error.to_string())),
        };
        if let Some(tree) = &tree {
            definition = serde_json::to_value(&tree.root().def).ok();
            if let Some(serde_json::Value::Object(object)) = definition.as_mut() {
                object.insert(
                    "schemaVersion".to_string(),
                    serde_json::Value::from(crate::team::TEAM_SCHEMA_VERSION),
                );
            }
        }

        let live = self.session.agents.list();
        let mut members: Vec<TeamMemberSnapshot> = tree
            .as_ref()
            .map(|tree| {
                tree.members()
                    .map(|(node, member)| {
                        let status = live.iter().find(|status| status.name == member.name);
                        let definition =
                            crate::agents::load_agent_defs(&self.session.home, &node.dir)
                                .into_iter()
                                .find(|definition| definition.name == member.agent);
                        let runtime_status = status
                            .map(|status| match status.state {
                                crate::agents::AgentState::Running => "busy",
                                crate::agents::AgentState::Idle => "standby",
                                crate::agents::AgentState::Stopped => "failed",
                            })
                            .unwrap_or("offline");
                        let active_task =
                            self.session.team_tasks.active_task_for_member(&member.name);
                        let merged_profile = crate::team::MemberProfile::merged(
                            &definition
                                .as_ref()
                                .map(|definition| definition.profile.clone())
                                .unwrap_or_default(),
                            &member.profile,
                        );
                        let expected_model = member
                            .model
                            .clone()
                            .or_else(|| {
                                definition
                                    .as_ref()
                                    .and_then(|definition| definition.model.clone())
                            })
                            .unwrap_or_else(|| self.session.runtime.model.borrow().clone());
                        let expected_provider = member
                            .provider
                            .clone()
                            .or_else(|| {
                                definition
                                    .as_ref()
                                    .and_then(|definition| definition.provider.clone())
                            })
                            .unwrap_or_else(|| self.session.runtime.provider.borrow().clone());
                        let expected_thinking = member
                            .thinking
                            .clone()
                            .or_else(|| {
                                definition
                                    .as_ref()
                                    .and_then(|definition| definition.thinking.clone())
                            })
                            .or_else(|| self.session.runtime.thinking.borrow().clone());
                        let configuration_key = definition.as_ref().map(|definition| {
                            crate::team::runtime_member_configuration_key(
                                &member.member_id,
                                &member.agent,
                                &definition.system,
                                definition.inherit_system,
                                &expected_provider,
                                &expected_model,
                                expected_thinking.as_deref(),
                                &merged_profile,
                            )
                        });
                        TeamMemberSnapshot {
                            member_id: Some(member.member_id.clone()),
                            name: member.name.clone(),
                            agent: member.agent.clone(),
                            avatar: member.avatar.clone(),
                            avatar_data_url: avatar_thumbnail_data_url(
                                &node.dir,
                                member.avatar.as_deref(),
                            ),
                            status: runtime_status.to_string(),
                            pending: status.map(|status| status.pending).unwrap_or(0),
                            unacked: status.map(|status| status.unacked).unwrap_or(0),
                            model: status
                                .map(|status| status.model.clone())
                                .or_else(|| member.model.clone())
                                .or_else(|| {
                                    definition
                                        .as_ref()
                                        .and_then(|definition| definition.model.clone())
                                })
                                .unwrap_or_else(|| self.session.runtime.model.borrow().clone()),
                            provider: status
                                .map(|status| status.provider.clone())
                                .or_else(|| member.provider.clone())
                                .or_else(|| {
                                    definition
                                        .as_ref()
                                        .and_then(|definition| definition.provider.clone())
                                })
                                .unwrap_or_else(|| self.session.runtime.provider.borrow().clone()),
                            thinking: status
                                .and_then(|status| status.thinking.clone())
                                .or_else(|| member.thinking.clone())
                                .or_else(|| {
                                    definition
                                        .as_ref()
                                        .and_then(|definition| definition.thinking.clone())
                                })
                                .or_else(|| self.session.runtime.thinking.borrow().clone()),
                            profile: Some(merged_profile),
                            kind: "crew".to_string(),
                            recommended: false,
                            task_id: active_task.as_ref().map(|task| task.id.clone()),
                            restart_required: active_task.is_none()
                                && status
                                    .and_then(|status| status.configuration_key.as_ref())
                                    .zip(configuration_key.as_ref())
                                    .is_some_and(|(actual, expected)| actual != expected),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let fixed = members
            .iter()
            .map(|member| member.name.clone())
            .collect::<std::collections::HashSet<_>>();
        members.extend(
            live.iter()
                .filter(|status| {
                    status.kind == crate::agents::AgentKind::Hire && !fixed.contains(&status.name)
                })
                .map(|status| TeamMemberSnapshot {
                    member_id: None,
                    name: status.name.clone(),
                    agent: status
                        .def
                        .clone()
                        .unwrap_or_else(|| "temporary".to_string()),
                    avatar: None,
                    avatar_data_url: None,
                    status: match status.state {
                        crate::agents::AgentState::Running => "busy",
                        crate::agents::AgentState::Idle => "standby",
                        crate::agents::AgentState::Stopped => "failed",
                    }
                    .to_string(),
                    pending: status.pending,
                    unacked: status.unacked,
                    model: status.model.clone(),
                    provider: status.provider.clone(),
                    thinking: status.thinking.clone(),
                    profile: None,
                    kind: "hire".to_string(),
                    recommended: status.successful_runs >= 2 || status.useful,
                    task_id: status.task_id.clone(),
                    restart_required: false,
                }),
        );

        let channels = self
            .session
            .channels
            .list()
            .into_iter()
            .filter(|channel| {
                self.session
                    .team_tasks
                    .task_for_channel(&channel.name)
                    .is_none()
            })
            .map(|channel| TeamChannelSnapshot {
                messages: self.session.channels.log_of(&channel.name),
                name: channel.name,
                mode: channel.mode.label().to_string(),
                seq: channel.seq,
                frozen: channel.frozen,
                members: channel.members,
            })
            .collect();
        let agent_definitions = defs
            .into_iter()
            .filter_map(|definition| {
                let source = match definition.source {
                    crate::agents::AgentDefSource::Project => "project",
                    crate::agents::AgentDefSource::User => "user",
                    crate::agents::AgentDefSource::Unknown => return None,
                };
                Some(AgentDefinitionSnapshot {
                    name: definition.name,
                    description: definition.description,
                    source: source.to_string(),
                    model: definition.model,
                    provider: definition.provider,
                    thinking: definition.thinking,
                    profile: definition.profile,
                })
            })
            .collect();

        Ok(TeamSnapshot {
            available: raw.is_some(),
            path: path.display().to_string(),
            revision,
            branch: crate::team::current_branch(&project_dir),
            validation,
            definition,
            agent_definitions,
            avatars: {
                let mut avatars = crate::tui::avatar::ids()
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if let Some(tree) = &tree {
                    for node in tree.nodes() {
                        avatars
                            .extend(crate::team::project_avatar_ids(&node.dir).unwrap_or_default());
                    }
                } else {
                    avatars
                        .extend(crate::team::project_avatar_ids(&project_dir).unwrap_or_default());
                }
                avatars.sort();
                avatars.dedup();
                avatars
            },
            members,
            channels,
        })
    }

    pub(super) fn refresh_team(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        match self.team_snapshot() {
            Ok(snapshot) => self.emit(CliEvent::TeamSnapshot {
                base: EventBase::default(),
                command_id: Some(command_id),
                snapshot,
            }),
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error,
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn validate_team(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        match self.team_snapshot() {
            Ok(snapshot) => {
                let valid = snapshot.available && snapshot.validation.is_none();
                let msg = if !snapshot.available {
                    format!("{} is not configured", crate::team::TEAM_FILE)
                } else {
                    snapshot
                        .validation
                        .clone()
                        .unwrap_or_else(|| "team definition is valid".to_string())
                };
                self.emit(CliEvent::TeamValidation {
                    base: EventBase::default(),
                    command_id,
                    valid,
                    msg,
                })
            }
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error,
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn save_team(
        &mut self,
        command_id: String,
        base_revision: &str,
        mut definition: serde_json::Value,
    ) -> Result<(), JsonEventsError> {
        if !self.require_idle(&command_id, "team.save")? {
            return Ok(());
        }
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let _team_lock = match crate::team::lock_team_file(&project_dir) {
            Ok(lock) => lock,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "STORAGE_ERROR",
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let path = project_dir.join(crate::team::TEAM_FILE);
        let existing = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "STORAGE_ERROR",
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if crate::update::sha256_hex(&existing) != base_revision {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_CONFLICT",
                "team definition changed on disk; refresh before saving",
                EventErrorLevel::Page,
                true,
            );
        }
        let previous_member_ids = crate::team::load_team_file(&project_dir)
            .ok()
            .flatten()
            .map(|definition| {
                definition
                    .members
                    .into_iter()
                    .map(|member| member.member_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let serde_json::Value::Object(object) = &mut definition {
            object.insert(
                "schemaVersion".to_string(),
                serde_json::Value::from(crate::team::TEAM_SCHEMA_VERSION),
            );
        }
        let parsed: crate::team::TeamDef = match serde_json::from_value(definition.clone()) {
            Ok(parsed) => parsed,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("team definition is invalid: {error}"),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let tree = match crate::team::build_tree(parsed, &project_dir) {
            Ok(tree) => tree,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if let Err(error) = crate::team::validate_tree(&tree, &self.session, &self.session.home) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        definition = serde_json::to_value(&tree.root().def)?;
        definition["schemaVersion"] = serde_json::Value::from(crate::team::TEAM_SCHEMA_VERSION);
        let existing_value = serde_json::from_slice(&existing).unwrap_or(serde_json::Value::Null);
        let merged = merge_team_value(existing_value, definition);
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let mut encoded = match serde_json::to_string_pretty(&merged) {
            Ok(encoded) => encoded,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "STORAGE_ERROR",
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        encoded.push('\n');
        let current_member_ids = tree
            .root()
            .def
            .members
            .iter()
            .map(|member| member.member_id.clone())
            .collect::<Vec<_>>();
        if let Err(error) = crate::team::reconcile_member_experience(
            &self.session.home,
            &project_dir,
            &previous_member_ids,
            &current_member_ids,
        ) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        if let Err(error) = crate::storage::write_atomic(&path, encoded.as_bytes()) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::TeamUpdated {
            base: EventBase::default(),
            command_id,
            action: "saved".to_string(),
            msg: "team definition saved".to_string(),
            snapshot,
        })
    }

    pub(super) fn start_team(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        if !self.require_idle(&command_id, "team.start")? {
            return Ok(());
        }
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let tree = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("{} is not configured", crate::team::TEAM_FILE),
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let summary = match crate::team::spawn_tree(&self.session, &tree, &self.session.home) {
            Ok(summary) => summary,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::TeamUpdated {
            base: EventBase::default(),
            command_id,
            action: "started".to_string(),
            msg: format!(
                "team started: {} spawned, {} reused, {} failed",
                summary.spawned.len(),
                summary.reused.len(),
                summary.failed.len()
            ),
            snapshot,
        })
    }

    pub(super) fn stop_team(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        if !self.require_idle(&command_id, "team.stop")? {
            return Ok(());
        }
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let tree = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("{} is not configured", crate::team::TEAM_FILE),
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let mut stopped = 0usize;
        for (_, member) in tree.members() {
            if let Ok((watch_id, _)) = self.session.agents.stop(&member.name) {
                if let Some(watch_id) = watch_id {
                    self.session.watch.set_state(
                        watch_id,
                        crate::watch::WatchState::Cancelled,
                        Some("stopped".to_string()),
                        None,
                    );
                }
                stopped += 1;
            }
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::TeamUpdated {
            base: EventBase::default(),
            command_id,
            action: "stopped".to_string(),
            msg: format!("stopped {stopped} team members"),
            snapshot,
        })
    }

    pub(super) fn get_team_lobby(
        &mut self,
        command_id: String,
        before_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<(), JsonEventsError> {
        self.emit(CliEvent::TeamLobbySnapshot {
            base: EventBase::default(),
            command_id: Some(command_id),
            lobby: self.session.team_tasks.lobby(before_seq, limit),
        })
    }

    pub(super) fn post_team_lobby(
        &mut self,
        command_id: String,
        text: &str,
        requested_targets: &[String],
    ) -> Result<(), JsonEventsError> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let tree = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("{} is not configured", crate::team::TEAM_FILE),
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if let Err(error) = crate::team::spawn_tree(&self.session, &tree, &self.session.home) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let all_members = tree
            .members()
            .map(|(_, member)| member.name.clone())
            .collect::<Vec<_>>();
        let targets = if requested_targets.is_empty() {
            let mentioned = all_members
                .iter()
                .filter(|member| text.contains(&format!("@{member}")))
                .cloned()
                .collect::<Vec<_>>();
            if mentioned.is_empty() {
                all_members.clone()
            } else {
                mentioned
            }
        } else {
            let unknown = requested_targets
                .iter()
                .filter(|target| !all_members.contains(target))
                .cloned()
                .collect::<Vec<_>>();
            if !unknown.is_empty() {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &format!("unknown team members: {}", unknown.join(", ")),
                    EventErrorLevel::Page,
                    true,
                );
            }
            requested_targets.to_vec()
        };
        if let Err(error) = self.session.team_tasks.record_lobby_message(
            "user",
            Some(crate::channels::USER_NAME),
            &targets,
            text,
        ) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let running = self
            .session
            .agents
            .list()
            .into_iter()
            .filter(|agent| agent.state == crate::agents::AgentState::Running)
            .map(|agent| agent.name)
            .collect::<std::collections::HashSet<_>>();
        let mut delivered = Vec::new();
        let mut skipped = Vec::new();
        for target in targets {
            if running.contains(&target)
                || self
                    .session
                    .team_tasks
                    .active_task_for_member(&target)
                    .is_some()
            {
                skipped.push(target);
                continue;
            }
            match self.session.agents.deliver(
                &target,
                crate::channels::USER_NAME,
                text,
                Vec::new(),
                None,
            ) {
                Ok(_) => delivered.push(target),
                Err(_) => skipped.push(target),
            }
        }
        crate::tool::agent::flush_agent_inbox(&self.session, &self.session.watch);
        if !skipped.is_empty() {
            let _ = self.session.team_tasks.record_lobby_message(
                "system",
                None,
                &[],
                &format!(
                    "Skipped busy or unavailable members: {}",
                    skipped.join(", ")
                ),
            );
        }
        let lobby = self.session.team_tasks.lobby(None, Some(100));
        self.emit(CliEvent::TeamLobbySnapshot {
            base: EventBase::default(),
            command_id: Some(command_id),
            lobby,
        })
    }

    pub(super) fn import_team_avatar(
        &mut self,
        command_id: String,
        file_name: &str,
        data: &str,
    ) -> Result<(), JsonEventsError> {
        if file_name.trim().is_empty() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                "avatar fileName must not be empty",
                EventErrorLevel::Field,
                true,
            );
        }
        let bytes = match BASE64.decode(data) {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &format!("avatar data is not valid base64: {error}"),
                    EventErrorLevel::Field,
                    true,
                );
            }
        };
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let avatar = match crate::team::import_avatar(&project_dir, &bytes) {
            Ok(avatar) => avatar,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Field,
                    true,
                );
            }
        };
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::TeamAvatarImported {
            base: EventBase::default(),
            command_id,
            avatar,
            snapshot,
        })
    }

    pub(super) fn get_team_avatar(
        &mut self,
        command_id: String,
        avatar: &str,
    ) -> Result<(), JsonEventsError> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let tree = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("{} is not configured", crate::team::TEAM_FILE),
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let path = tree.nodes().iter().find_map(|node| {
            crate::team::project_avatar_path(&node.dir, avatar).filter(|path| path.is_file())
        });
        let Some(path) = path else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_INVALID",
                "avatar is not available in the current team tree",
                EventErrorLevel::Field,
                true,
            );
        };
        let encoded = match std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                let image = image::load_from_memory(&bytes).map_err(|error| error.to_string())?;
                let thumbnail =
                    image.resize_to_fill(128, 128, image::imageops::FilterType::Lanczos3);
                let mut output = std::io::Cursor::new(Vec::new());
                thumbnail
                    .write_to(&mut output, image::ImageFormat::Png)
                    .map_err(|error| error.to_string())?;
                Ok(output.into_inner())
            }) {
            Ok(encoded) => encoded,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "STORAGE_ERROR",
                    &format!("failed to read team avatar: {error}"),
                    EventErrorLevel::Field,
                    true,
                );
            }
        };
        self.emit(CliEvent::TeamAvatarLoaded {
            base: EventBase::default(),
            command_id,
            avatar: avatar.to_string(),
            data_url: format!("data:image/png;base64,{}", BASE64.encode(encoded)),
        })
    }

    pub(super) fn inspect_team_preset(
        &mut self,
        command_id: String,
        data: &str,
    ) -> Result<(), JsonEventsError> {
        let bytes = match BASE64.decode(data) {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &format!("team preset data is not valid base64: {error}"),
                    EventErrorLevel::Field,
                    true,
                );
            }
        };
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        match crate::team_presets::inspect(&self.session.home, &project_dir, &bytes) {
            Ok(preview) => self.emit(CliEvent::TeamPresetPreview {
                base: EventBase::default(),
                command_id,
                preview,
            }),
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn import_team_preset(
        &mut self,
        command_id: String,
        data: &str,
        base_revision: &str,
        resolutions: &std::collections::HashMap<String, String>,
        model_mappings: &std::collections::HashMap<
            String,
            crate::team_presets::TeamPresetModelMapping,
        >,
    ) -> Result<(), JsonEventsError> {
        if !self.require_idle(&command_id, "team.preset.import")? {
            return Ok(());
        }
        let bytes = match BASE64.decode(data) {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &format!("team preset data is not valid base64: {error}"),
                    EventErrorLevel::Field,
                    true,
                );
            }
        };
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        match crate::team_presets::import(
            &self.session,
            &self.session.home,
            &project_dir,
            &bytes,
            base_revision,
            resolutions,
            model_mappings,
        ) {
            Ok(preview) => {
                let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
                self.emit(CliEvent::TeamPresetImported {
                    base: EventBase::default(),
                    command_id,
                    preview,
                    snapshot,
                })
            }
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn export_team_preset(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        match crate::team_presets::export(&self.session.home, &project_dir) {
            Ok(bytes) => {
                let data = BASE64.encode(bytes);
                if data.len() > MAX_EVENT_LINE_BYTES.saturating_sub(4096) {
                    return self.emit_error(
                        ErrorScope::Command,
                        Some(command_id),
                        None,
                        "CONFIG_INVALID",
                        "team preset is too large for the JSON-events transport",
                        EventErrorLevel::Page,
                        true,
                    );
                }
                let name = project_dir
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("team");
                self.emit(CliEvent::TeamPresetExported {
                    base: EventBase::default(),
                    command_id,
                    file_name: format!("{name}.bingo-team"),
                    data,
                })
            }
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn mark_team_member_useful(
        &mut self,
        command_id: String,
        member: &str,
    ) -> Result<(), JsonEventsError> {
        if let Err(error) = self.session.agents.mark_useful(member) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                &error,
                EventErrorLevel::Page,
                true,
            );
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::TeamMemberConfigured {
            base: EventBase::default(),
            command_id,
            action: "useful".to_string(),
            member: member.to_string(),
            member_id: None,
            snapshot,
        })
    }

    pub(super) fn restart_team_member(
        &mut self,
        command_id: String,
        member: &str,
    ) -> Result<(), JsonEventsError> {
        if self
            .session
            .team_tasks
            .active_task_for_member(member)
            .is_some()
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "TEAM_MEMBER_BUSY",
                "member is reserved by an unfinished task",
                EventErrorLevel::Page,
                true,
            );
        }
        let status = self
            .session
            .agents
            .list()
            .into_iter()
            .find(|status| status.name == member);
        if status.as_ref().is_some_and(|status| {
            status.kind != crate::agents::AgentKind::Crew
                || status.state == crate::agents::AgentState::Running
        }) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "TEAM_MEMBER_BUSY",
                "only an idle fixed member can be restarted",
                EventErrorLevel::Page,
                true,
            );
        }
        if status.is_some() {
            let _ = self.session.agents.remove(member);
        }
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let tree = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    "team.json is not configured",
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if !tree
            .members()
            .any(|(_, candidate)| candidate.name == member)
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                "member is not part of the fixed team",
                EventErrorLevel::Page,
                true,
            );
        }
        if let Err(error) = crate::team::spawn_tree(&self.session, &tree, &self.session.home) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        let member_id = snapshot
            .members
            .iter()
            .find(|candidate| candidate.name == member)
            .and_then(|candidate| candidate.member_id.clone());
        self.emit(CliEvent::TeamMemberConfigured {
            base: EventBase::default(),
            command_id,
            action: "restarted".to_string(),
            member: member.to_string(),
            member_id,
            snapshot,
        })
    }

    pub(super) fn promote_team_member(
        &mut self,
        command_id: String,
        member: &str,
        base_revision: &str,
    ) -> Result<(), JsonEventsError> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let path = project_dir.join(crate::team::TEAM_FILE);
        let raw = std::fs::read(&path).unwrap_or_default();
        if crate::update::sha256_hex(&raw) != base_revision {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_CONFLICT",
                "team definition changed on disk; refresh before promoting the member",
                EventErrorLevel::Page,
                true,
            );
        }
        let status = match self
            .session
            .agents
            .list()
            .into_iter()
            .find(|status| status.name == member)
        {
            Some(status)
                if status.kind == crate::agents::AgentKind::Hire
                    && status.state != crate::agents::AgentState::Running =>
            {
                status
            }
            _ => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "TEAM_MEMBER_BUSY",
                    "only an idle temporary member can be promoted",
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        let Some(agent) = status.def.clone() else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_INVALID",
                "temporary member has no reusable Agent role",
                EventErrorLevel::Page,
                true,
            );
        };
        let mut definition = match crate::team::load_team_file(&project_dir) {
            Ok(Some(definition)) => definition,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    "team.json is not configured",
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if definition
            .members
            .iter()
            .any(|candidate| candidate.name == member)
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_CONFLICT",
                "a fixed member already uses this name",
                EventErrorLevel::Page,
                true,
            );
        }
        let member_id = format!(
            "member-{}",
            crate::update::sha256_hex(
                format!("{}\0{}", crate::team::project_key(&project_dir), member).as_bytes()
            )
            .chars()
            .take(24)
            .collect::<String>()
        );
        let avatar = crate::tui::avatar::random_default_id(
            definition
                .members
                .iter()
                .filter_map(|member| member.avatar.as_deref()),
            &member_id,
        );
        definition.members.push(crate::team::TeamMember {
            member_id: member_id.clone(),
            name: member.to_string(),
            agent,
            avatar: Some(avatar.to_string()),
            model: Some(status.model),
            provider: Some(status.provider),
            thinking: status.thinking,
            profile: crate::team::MemberProfile::default(),
        });
        if let Err(error) = crate::team::write_team_file(&project_dir, &definition) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let _ = self.session.agents.remove(member);
        let tree = crate::team::load_team_tree(&project_dir)
            .map_err(|error| JsonEventsError::BadArgument(error.to_string()))?
            .ok_or_else(|| {
                JsonEventsError::BadArgument("team.json is not configured".to_string())
            })?;
        crate::team::spawn_tree(&self.session, &tree, &self.session.home)
            .map_err(|error| JsonEventsError::BadArgument(error.to_string()))?;
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::TeamMemberConfigured {
            base: EventBase::default(),
            command_id,
            action: "promoted".to_string(),
            member: member.to_string(),
            member_id: Some(member_id),
            snapshot,
        })
    }

    pub(super) fn list_team_tasks(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        self.emit(CliEvent::TeamTasksSnapshot {
            base: EventBase::default(),
            command_id: Some(command_id),
            branch: self.session.team_tasks.branch().to_string(),
            tasks: self.session.team_tasks.list(),
        })
    }

    pub(super) fn get_team_task(
        &mut self,
        command_id: String,
        task_id: &str,
        before_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<(), JsonEventsError> {
        match self.session.team_tasks.get(task_id, before_seq, limit) {
            Ok(detail) => self.emit(CliEvent::TeamTaskUpdated {
                base: EventBase::default(),
                command_id: Some(command_id),
                action: "loaded".to_string(),
                task: crate::team_tasks::TeamTaskSummary::from(&detail),
                detail: Some(detail),
            }),
            Err(error) => self.emit_team_task_error(command_id, error),
        }
    }

    pub(super) fn create_team_task(
        &mut self,
        request: TeamTaskCreateRequest,
    ) -> Result<(), JsonEventsError> {
        let TeamTaskCreateRequest {
            command_id,
            title,
            description,
            participants: requested,
            leader: requested_leader,
            context_message_seqs,
            additional_constraints,
        } = request;
        if !self.require_idle(&command_id, "team.task.create")? {
            return Ok(());
        }
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let tree = match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("{} is not configured", crate::team::TEAM_FILE),
                    EventErrorLevel::Page,
                    true,
                );
            }
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if let Err(error) = crate::team::validate_tree(&tree, &self.session, &self.session.home) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let requested = requested.unwrap_or_else(|| {
            tree.members()
                .map(|(_, member)| member.name.clone())
                .collect()
        });
        let selected = requested.iter().collect::<std::collections::HashSet<_>>();
        if selected.len() != requested.len() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                "participants must not contain duplicate member names",
                EventErrorLevel::Field,
                true,
            );
        }
        let live = self.session.agents.list();
        let mut participants = Vec::new();
        for name in &requested {
            let Some((node, member)) = tree.find_member(name) else {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &format!("unknown team member {name}"),
                    EventErrorLevel::Field,
                    true,
                );
            };
            if live.iter().any(|agent| {
                agent.name == *name && agent.state == crate::agents::AgentState::Running
            }) {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "TEAM_MEMBER_BUSY",
                    &format!("team member {name} is currently running"),
                    EventErrorLevel::Page,
                    true,
                );
            }
            let defs = crate::agents::load_agent_defs(&self.session.home, &node.dir);
            let Some(definition) = defs
                .iter()
                .find(|definition| definition.name == member.agent)
            else {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &format!("member {name} references missing AgentDef {}", member.agent),
                    EventErrorLevel::Page,
                    true,
                );
            };
            participants.push(crate::team_tasks::TeamTaskMember {
                member_id: member.member_id.clone(),
                name: member.name.clone(),
                agent: member.agent.clone(),
                description: definition.description.clone(),
                system: definition.system.clone(),
                inherit_system: definition.inherit_system,
                avatar: member.avatar.clone(),
                model: Some(
                    member
                        .model
                        .clone()
                        .or_else(|| definition.model.clone())
                        .unwrap_or_else(|| self.session.runtime.model.borrow().clone()),
                ),
                provider: Some(
                    member
                        .provider
                        .clone()
                        .or_else(|| definition.provider.clone())
                        .unwrap_or_else(|| self.session.runtime.provider.borrow().clone()),
                ),
                thinking: Some(
                    member
                        .thinking
                        .clone()
                        .or_else(|| definition.thinking.clone())
                        .or_else(|| self.session.runtime.thinking.borrow().clone())
                        .unwrap_or_else(|| "off".to_string()),
                ),
                profile: crate::team::MemberProfile::merged(&definition.profile, &member.profile)
                    .with_constraints(&additional_constraints),
                team: node.def.name.clone(),
                directory: node.dir.display().to_string(),
            });
        }
        if let Some(leader) = requested_leader.as_deref()
            && !requested.iter().any(|member| member == leader)
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                "leader must be one of the selected participants",
                EventErrorLevel::Field,
                true,
            );
        }
        let leader = requested_leader
            .or_else(|| tree.root().def.leader.clone())
            .filter(|leader| requested.iter().any(|member| member == leader))
            .or_else(|| requested.first().cloned())
            .unwrap_or_default();
        let context_messages = match self.session.team_tasks.lobby_context(&context_message_seqs) {
            Ok(messages) => messages,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        let task = match self
            .session
            .team_tasks
            .create(crate::team_tasks::CreateTeamTask {
                team: tree.root().def.name.clone(),
                title,
                description: description.clone(),
                participants,
                leader,
                context_message_seqs,
                context_messages,
                additional_constraints,
            }) {
            Ok(task) => task,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };

        for member in &task.participants {
            if self
                .session
                .agents
                .state_in_project(&member.name, std::path::Path::new(&member.directory))
                .is_some()
            {
                let _ = self.session.agents.remove(&member.name);
                self.session.channels.remove_member_everywhere(&member.name);
            }
        }
        if let Err(error) = self.ensure_task_members(&task) {
            let _ = self.session.team_tasks.cancel(&task.id);
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_INVALID",
                &error,
                EventErrorLevel::Page,
                true,
            );
        }
        if let Err(error) = crate::tool::channel::deliver_post(
            &self.session,
            &self.session.watch,
            crate::channels::USER_NAME,
            &task.channel,
            &description,
        ) {
            let _ = self.session.team_tasks.cancel(&task.id);
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error,
                EventErrorLevel::Page,
                true,
            );
        }
        let detail = self
            .session
            .team_tasks
            .get(&task.id, None, Some(100))
            .map_err(|error| JsonEventsError::BadArgument(error.to_string()))?;
        self.emit(CliEvent::TeamTaskUpdated {
            base: EventBase::default(),
            command_id: Some(command_id),
            action: "created".to_string(),
            task: crate::team_tasks::TeamTaskSummary::from(&detail),
            detail: Some(detail),
        })
    }

    pub(super) fn post_team_task(
        &mut self,
        command_id: String,
        task_id: &str,
        text: &str,
    ) -> Result<(), JsonEventsError> {
        let task = match self.session.team_tasks.get(task_id, None, Some(1)) {
            Ok(task) => task,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        if let Err(error) = self
            .session
            .team_tasks
            .accepts_post(&task.channel, crate::channels::USER_NAME)
        {
            return self.emit_team_task_error(command_id, error);
        }
        if let Some(channel) = self.session.channels.info(&task.channel) {
            self.session
                .channels
                .mark_seen(crate::channels::USER_NAME, &task.channel, channel.seq);
        }
        match crate::tool::channel::deliver_post(
            &self.session,
            &self.session.watch,
            crate::channels::USER_NAME,
            &task.channel,
            text,
        ) {
            Ok(crate::tool::channel::PostDelivery::Sent { .. }) => {
                let task = self
                    .session
                    .team_tasks
                    .get(task_id, None, Some(1))
                    .map_err(|error| JsonEventsError::BadArgument(error.to_string()))?;
                self.emit(CliEvent::TeamTaskUpdated {
                    base: EventBase::default(),
                    command_id: Some(command_id),
                    action: "posted".to_string(),
                    task: crate::team_tasks::TeamTaskSummary::from(&task),
                    detail: None,
                })
            }
            Ok(crate::tool::channel::PostDelivery::Stale { .. }) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_CONFLICT",
                "task conversation advanced while posting; retry with the latest messages",
                EventErrorLevel::Page,
                true,
            ),
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                if error.contains("belongs to branch") {
                    "TASK_BRANCH_MISMATCH"
                } else if error.contains("is already assigned") {
                    "TEAM_MEMBER_BUSY"
                } else if error.contains("storage failed") || error.contains("serialization failed")
                {
                    "STORAGE_ERROR"
                } else {
                    "TASK_INVALID_STATE"
                },
                &error,
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn pause_team_task(
        &mut self,
        command_id: String,
        task_id: &str,
    ) -> Result<(), JsonEventsError> {
        let task = match self.session.team_tasks.get(task_id, None, Some(1)) {
            Ok(task) => task,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        let summary = match self.session.team_tasks.begin_pause(
            task_id,
            crate::team_tasks::TeamTaskStatus::Paused,
            "Paused by user".to_string(),
            None,
        ) {
            Ok(summary) => summary,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        let _ = self
            .session
            .team_tasks
            .record_system(task_id, "Pause requested by user");
        for member in &task.participants {
            self.session
                .agents
                .discard_channel_inbox(&member.name, &task.channel);
        }
        let settled = self
            .session
            .team_tasks
            .settle_ready_tasks(&self.session.agents)
            .map_err(|error| JsonEventsError::BadArgument(error.to_string()))?;
        let summary = settled
            .into_iter()
            .find(|task| task.id == task_id)
            .unwrap_or(summary);
        self.emit(CliEvent::TeamTaskUpdated {
            base: EventBase::default(),
            command_id: Some(command_id),
            action: "paused".to_string(),
            task: summary,
            detail: None,
        })
    }

    pub(super) fn resume_team_task(
        &mut self,
        command_id: String,
        task_id: &str,
        message: Option<&str>,
    ) -> Result<(), JsonEventsError> {
        let task = match self.session.team_tasks.get(task_id, None, Some(1)) {
            Ok(task) => task,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        if !matches!(
            task.status,
            crate::team_tasks::TeamTaskStatus::Paused
                | crate::team_tasks::TeamTaskStatus::AwaitingReview
        ) {
            return self.emit_team_task_error(
                command_id,
                crate::team_tasks::TeamTaskError::InvalidState {
                    task_id: task.id,
                    status: task.status,
                    operation: "resume",
                },
            );
        }
        if let Err(error) = self.session.team_tasks.require_current_branch(task_id) {
            return self.emit_team_task_error(command_id, error);
        }
        if let Err(error) = self.ensure_task_members(&task) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_INVALID",
                &error,
                EventErrorLevel::Page,
                true,
            );
        }
        let summary = match self.session.team_tasks.resume(task_id) {
            Ok(summary) => summary,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        let _ = self
            .session
            .team_tasks
            .record_system(task_id, "Task resumed by user");
        if let Some(channel) = self.session.channels.info(&task.channel) {
            self.session
                .channels
                .mark_seen(crate::channels::USER_NAME, &task.channel, channel.seq);
        }
        let prompt = message
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .unwrap_or("Continue the task from its durable transcript.");
        if let Err(error) = crate::tool::channel::deliver_post(
            &self.session,
            &self.session.watch,
            crate::channels::USER_NAME,
            &task.channel,
            prompt,
        ) {
            let _ = self.session.team_tasks.begin_pause(
                task_id,
                crate::team_tasks::TeamTaskStatus::Paused,
                "Resume delivery failed".to_string(),
                None,
            );
            let _ = self
                .session
                .team_tasks
                .settle_ready_tasks(&self.session.agents);
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                &error,
                EventErrorLevel::Page,
                true,
            );
        }
        self.emit(CliEvent::TeamTaskUpdated {
            base: EventBase::default(),
            command_id: Some(command_id),
            action: "resumed".to_string(),
            task: summary,
            detail: None,
        })
    }

    pub(super) fn complete_team_task(
        &mut self,
        command_id: String,
        task_id: &str,
    ) -> Result<(), JsonEventsError> {
        let detail = match self.session.team_tasks.get(task_id, None, Some(1)) {
            Ok(task) => task,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        match self.session.team_tasks.complete(task_id) {
            Ok(task) => {
                let _ = self
                    .session
                    .team_tasks
                    .record_system(task_id, "Task completed by user");
                let experience_summary = detail
                    .review_summary
                    .as_deref()
                    .unwrap_or(&detail.description)
                    .to_string();
                let project_dir = std::path::PathBuf::from(&detail.project_path);
                for member in detail.participants {
                    if let Err(error) = crate::team::append_member_experience(
                        &self.session.home,
                        &project_dir,
                        &member.member_id,
                        &detail.id,
                        &detail.title,
                        &experience_summary,
                    ) {
                        let _ = self.session.team_tasks.record_system(
                            task_id,
                            format!(
                                "Could not save confirmed experience for {}: {error}",
                                member.name
                            ),
                        );
                    }
                    let _ = self.session.agents.stop(&member.name);
                }
                self.emit(CliEvent::TeamTaskUpdated {
                    base: EventBase::default(),
                    command_id: Some(command_id),
                    action: "completed".to_string(),
                    task,
                    detail: None,
                })
            }
            Err(error) => self.emit_team_task_error(command_id, error),
        }
    }

    pub(super) fn cancel_team_task(
        &mut self,
        command_id: String,
        task_id: &str,
    ) -> Result<(), JsonEventsError> {
        let task = match self.session.team_tasks.get(task_id, None, Some(1)) {
            Ok(task) => task,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        let summary = match self.session.team_tasks.cancel(task_id) {
            Ok(summary) => summary,
            Err(error) => return self.emit_team_task_error(command_id, error),
        };
        let _ = self
            .session
            .team_tasks
            .record_system(task_id, "Task cancelled by user");
        for member in task.participants {
            let _ = self.session.agents.stop(&member.name);
        }
        self.emit(CliEvent::TeamTaskUpdated {
            base: EventBase::default(),
            command_id: Some(command_id),
            action: "cancelled".to_string(),
            task: summary,
            detail: None,
        })
    }

    pub(super) fn ensure_task_members(
        &self,
        task: &crate::team_tasks::TeamTask,
    ) -> Result<(), String> {
        let mut created = Vec::new();
        let result = (|| {
            for member in &task.participants {
                let cwd = std::path::PathBuf::from(&member.directory);
                if self
                    .session
                    .agents
                    .state_in_project(&member.name, &cwd)
                    .is_some_and(|state| state != crate::agents::AgentState::Stopped)
                {
                    continue;
                }
                if self
                    .session
                    .agents
                    .state_in_project(&member.name, &cwd)
                    .is_some()
                {
                    let _ = self.session.agents.remove(&member.name);
                    self.session.channels.remove_member_everywhere(&member.name);
                }
                if self.session.agents.claim_name(&member.name) != member.name {
                    return Err(format!(
                        "cannot restore task member {}; the instance name is in use",
                        member.name
                    ));
                }
                let definition = crate::agents::AgentDef {
                    name: member.agent.clone(),
                    description: member.description.clone(),
                    model: member.model.clone(),
                    provider: member.provider.clone(),
                    thinking: member.thinking.clone(),
                    system: member.system.clone(),
                    inherit_system: member.inherit_system,
                    profile: crate::team::MemberProfile::default(),
                    source: crate::agents::AgentDefSource::Unknown,
                };
                let branch = crate::team::current_branch(&cwd);
                crate::team::ensure_transcript(
                    &self.session.home,
                    &cwd,
                    &branch,
                    &member.team,
                    &member.name,
                );
                let context = crate::tool::agent::MemberContext {
                    memory: crate::team::member_memory_note(
                        &self.session.home,
                        &cwd,
                        &branch,
                        &member.team,
                        &member.name,
                    ),
                    norms: crate::team::load_norms(&cwd)
                        .map(|norms| crate::team::norms_block(&member.team, &norms)),
                    standing: self.session.team_tasks.member_context_note(&member.name),
                    profile: Some(member.profile.clone()),
                    experience: crate::team::member_experience_note(
                        &self.session.home,
                        &cwd,
                        &member.member_id,
                    ),
                    cwd: Some(cwd),
                };
                let sub = crate::tool::agent::build_sub_session(
                    &self.session,
                    member.model.clone(),
                    member.provider.clone(),
                    member.thinking.clone(),
                    Some(&definition),
                    &member.name,
                    context,
                )
                .map_err(|error| error.to_string())?;
                self.session.agents.insert(
                    &member.name,
                    crate::agents::AgentKind::Crew,
                    Some(member.agent.clone()),
                    member.description.clone(),
                    sub,
                );
                let configuration_key = crate::team::runtime_member_configuration_key(
                    &member.member_id,
                    &member.agent,
                    &member.system,
                    member.inherit_system,
                    member.provider.as_deref().unwrap_or("default"),
                    member.model.as_deref().unwrap_or_default(),
                    member.thinking.as_deref(),
                    &member.profile,
                );
                self.session
                    .agents
                    .set_configuration_key(&member.name, configuration_key);
                self.session.agents.mark_idle(&member.name);
                created.push(member.name.clone());
            }
            if self.session.channels.info(&task.channel).is_none() {
                self.session.channels.create(
                    &task.channel,
                    task.participants
                        .iter()
                        .map(|member| member.name.clone())
                        .collect(),
                    crate::channels::ChannelMode::Serial,
                )?;
            } else {
                let existing = self
                    .session
                    .channels
                    .info(&task.channel)
                    .map(|channel| channel.members)
                    .unwrap_or_default();
                for member in &task.participants {
                    if !existing.iter().any(|name| name == &member.name) {
                        self.session.channels.invite(&task.channel, &member.name)?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            for member in created {
                let _ = self.session.agents.remove(&member);
                self.session.channels.remove_member_everywhere(&member);
            }
        }
        result
    }

    pub(super) fn emit_team_task_error(
        &mut self,
        command_id: String,
        error: crate::team_tasks::TeamTaskError,
    ) -> Result<(), JsonEventsError> {
        self.emit_error(
            ErrorScope::Command,
            Some(command_id),
            None,
            error.error_code(),
            &error.to_string(),
            EventErrorLevel::Page,
            true,
        )
    }

    pub(super) fn message_agent(
        &mut self,
        command_id: String,
        member: &str,
        message: &str,
    ) -> Result<(), JsonEventsError> {
        let message_id = match self.session.agents.deliver(
            member,
            crate::channels::USER_NAME,
            message,
            Vec::new(),
            None,
        ) {
            Ok(message_id) => message_id,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &error,
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        crate::tool::agent::flush_agent_inbox(&self.session, &self.session.watch);
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::AgentUpdated {
            base: EventBase::default(),
            command_id,
            action: "messaged".to_string(),
            member: member.to_string(),
            msg: format!("queued message #{message_id}"),
            snapshot,
        })
    }

    pub(super) fn stop_agent(
        &mut self,
        command_id: String,
        member: &str,
    ) -> Result<(), JsonEventsError> {
        let (watch_id, dropped) = match self.session.agents.stop(member) {
            Ok(result) => result,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &error,
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if let Some(watch_id) = watch_id {
            self.session.watch.set_state(
                watch_id,
                crate::watch::WatchState::Cancelled,
                Some("stopped".to_string()),
                None,
            );
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::AgentUpdated {
            base: EventBase::default(),
            command_id,
            action: "stopped".to_string(),
            member: member.to_string(),
            msg: format!("agent stopped; {dropped} queued messages discarded"),
            snapshot,
        })
    }

    pub(super) fn remove_agent(
        &mut self,
        command_id: String,
        member: &str,
    ) -> Result<(), JsonEventsError> {
        let (watch_id, dropped) = match self.session.agents.remove(member) {
            Ok(result) => result,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    &error,
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        self.session.channels.remove_member_everywhere(member);
        if let Some(watch_id) = watch_id {
            self.session.watch.set_state(
                watch_id,
                crate::watch::WatchState::Cancelled,
                Some("deleted".to_string()),
                None,
            );
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::AgentUpdated {
            base: EventBase::default(),
            command_id,
            action: "removed".to_string(),
            member: member.to_string(),
            msg: format!("agent removed; {dropped} queued messages discarded"),
            snapshot,
        })
    }

    pub(super) fn agent_activity(
        &mut self,
        command_id: String,
        member: &str,
    ) -> Result<(), JsonEventsError> {
        let Some(acks) = self.session.agents.acks_of(member) else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                &format!("no subagent named {member}"),
                EventErrorLevel::Page,
                true,
            );
        };
        let activity = acks
            .into_iter()
            .map(|ack| {
                let status = match ack.state {
                    crate::agents::AckState::Queued => "queued",
                    crate::agents::AckState::Delivered { .. } => "delivered",
                    crate::agents::AckState::Answered { .. } => "answered",
                    crate::agents::AckState::Dropped { .. } => "dropped",
                };
                AgentActivityItem {
                    id: ack.id.to_string(),
                    kind: "message".to_string(),
                    summary: ack.excerpt,
                    status: status.to_string(),
                }
            })
            .collect();
        self.emit(CliEvent::AgentActivity {
            base: EventBase::default(),
            command_id,
            member: member.to_string(),
            activity,
        })
    }

    pub(super) fn list_agent_definitions(
        &mut self,
        command_id: String,
    ) -> Result<(), JsonEventsError> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        match crate::agents::list_agent_definition_documents(&self.session.home, &project_dir) {
            Ok(definitions) => self.emit(CliEvent::AgentDefinitionsSnapshot {
                base: EventBase::default(),
                command_id,
                definitions,
            }),
            Err(error) => self.emit_agent_definition_error(command_id, error),
        }
    }

    pub(super) fn get_agent_definition(
        &mut self,
        command_id: String,
        scope: &str,
        id: &str,
    ) -> Result<(), JsonEventsError> {
        let scope = match crate::agents::AgentDefinitionScope::parse(scope) {
            Ok(scope) => scope,
            Err(error) => return self.emit_agent_definition_error(command_id, error),
        };
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        match crate::agents::get_agent_definition_document(
            &self.session.home,
            &project_dir,
            scope,
            id,
        ) {
            Ok(definition) => self.emit(CliEvent::AgentDefinitionUpdated {
                base: EventBase::default(),
                command_id,
                action: "loaded".to_string(),
                definition,
                archive_path: None,
            }),
            Err(error) => self.emit_agent_definition_error(command_id, error),
        }
    }

    pub(super) fn save_agent_definition(
        &mut self,
        command_id: String,
        scope: &str,
        id: &str,
        base_revision: Option<&str>,
        definition: crate::agents::AgentDefinitionInput,
    ) -> Result<(), JsonEventsError> {
        if !self.require_idle(&command_id, "agent.definition.save")? {
            return Ok(());
        }
        let scope = match crate::agents::AgentDefinitionScope::parse(scope) {
            Ok(scope) => scope,
            Err(error) => return self.emit_agent_definition_error(command_id, error),
        };
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        match crate::agents::save_agent_definition_document(
            &self.session.home,
            &project_dir,
            scope,
            id,
            base_revision,
            definition,
        ) {
            Ok(definition) => self.emit(CliEvent::AgentDefinitionUpdated {
                base: EventBase::default(),
                command_id,
                action: "saved".to_string(),
                definition,
                archive_path: None,
            }),
            Err(error) => self.emit_agent_definition_error(command_id, error),
        }
    }

    pub(super) fn archive_agent_definition(
        &mut self,
        command_id: String,
        scope: &str,
        id: &str,
        base_revision: &str,
    ) -> Result<(), JsonEventsError> {
        if !self.require_idle(&command_id, "agent.definition.archive")? {
            return Ok(());
        }
        let scope = match crate::agents::AgentDefinitionScope::parse(scope) {
            Ok(scope) => scope,
            Err(error) => return self.emit_agent_definition_error(command_id, error),
        };
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        let referenced_name = crate::agents::get_agent_definition_document(
            &self.session.home,
            &project_dir,
            scope,
            id,
        )
        .ok()
        .map(|definition| definition.name);
        if let Ok(Some(tree)) = crate::team::load_team_tree(&project_dir)
            && tree.members().any(|(_, member)| {
                member.agent == id || referenced_name.as_ref() == Some(&member.agent)
            })
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_INVALID",
                &format!("role {id} is referenced by the current project team"),
                EventErrorLevel::Page,
                true,
            );
        }
        match crate::agents::archive_agent_definition_document(
            &self.session.home,
            &project_dir,
            scope,
            id,
            base_revision,
        ) {
            Ok((definition, archive_path)) => self.emit(CliEvent::AgentDefinitionUpdated {
                base: EventBase::default(),
                command_id,
                action: "archived".to_string(),
                definition,
                archive_path: Some(archive_path.display().to_string()),
            }),
            Err(error) => self.emit_agent_definition_error(command_id, error),
        }
    }

    pub(super) fn emit_agent_definition_error(
        &mut self,
        command_id: String,
        error: crate::agents::AgentDefinitionError,
    ) -> Result<(), JsonEventsError> {
        self.emit_error(
            ErrorScope::Command,
            Some(command_id),
            None,
            error.error_code(),
            &error.to_string(),
            EventErrorLevel::Page,
            true,
        )
    }

    pub(super) fn post_channel(
        &mut self,
        command_id: String,
        channel: &str,
        text: &str,
    ) -> Result<(), JsonEventsError> {
        match crate::tool::channel::deliver_post(
            &self.session,
            &self.session.watch,
            crate::channels::USER_NAME,
            channel,
            text,
        ) {
            Ok(crate::tool::channel::PostDelivery::Sent { seq }) => {
                if let Some(message) = self
                    .session
                    .channels
                    .log_of(channel)
                    .into_iter()
                    .find(|message| message.seq == seq)
                {
                    self.emit(CliEvent::ChannelMessage {
                        base: EventBase::default(),
                        command_id: None,
                        channel: channel.to_string(),
                        message,
                    })?;
                }
                let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
                self.emit(CliEvent::ChannelUpdated {
                    base: EventBase::default(),
                    command_id,
                    channel: channel.to_string(),
                    msg: format!("posted message #{seq}"),
                    snapshot,
                })
            }
            Ok(crate::tool::channel::PostDelivery::Stale { missed }) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "CONFIG_CONFLICT",
                &format!(
                    "channel advanced while posting; refresh and retry ({} messages missed)",
                    missed.len()
                ),
                EventErrorLevel::Page,
                true,
            ),
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                &error,
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    pub(super) fn channel_history(
        &mut self,
        command_id: String,
        channel: &str,
    ) -> Result<(), JsonEventsError> {
        if self.session.channels.info(channel).is_none() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                &format!("no channel #{channel}"),
                EventErrorLevel::Page,
                true,
            );
        }
        let snapshot = self.team_snapshot().map_err(JsonEventsError::BadArgument)?;
        self.emit(CliEvent::ChannelUpdated {
            base: EventBase::default(),
            command_id,
            channel: channel.to_string(),
            msg: "channel history refreshed".to_string(),
            snapshot,
        })
    }
}
