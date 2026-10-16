use bingo_sdk::*;
use bingo_services::{agents_control as agents, rooms_control as rooms, tasks_write as tasks};
use std::{collections::BTreeSet, sync::Arc};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Agents,
    Rooms,
    Tasks,
    Team,
}
impl Tab {
    pub const ALL: [Self; 4] = [Self::Agents, Self::Rooms, Self::Tasks, Self::Team];

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.label() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Rooms => "rooms",
            Self::Tasks => "tasks",
            Self::Team => "team",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirmation {
    Stop(String),
    Dismiss(String),
    CloseRoom(String),
}
impl Confirmation {
    pub fn prompt(&self) -> String {
        match self {
            Self::Stop(name) => format!("Stop {name}?"),
            Self::Dismiss(name) => {
                format!("Dismiss {name} and delete its journal?")
            }
            Self::CloseRoom(name) => format!("Close {name}?"),
        }
    }
}
#[derive(Clone)]
pub struct Panel {
    pub tab: Tab,
    pub selected: usize,
    pub model: bool,
    pub confirm: Option<Confirmation>,
    pub follow: Option<Arc<crate::live::Follow>>,
    pub watching: BTreeSet<SessionId>,
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            tab: Tab::Agents,
            selected: 0,
            model: false,
            confirm: None,
            follow: None,
            watching: BTreeSet::new(),
        }
    }
}
impl Panel {
    // Rendering owns follow/watching; input owns navigation and confirmation.
    pub fn apply_input(&mut self, input: &Self) {
        if (self.tab, self.selected) != (input.tab, input.selected) {
            self.follow = None;
        }
        self.tab = input.tab;
        self.selected = input.selected;
        self.model = input.model;
        self.confirm.clone_from(&input.confirm);
    }
}
#[derive(Default)]
pub struct Rows {
    pub agents: Option<Vec<agents::AgentRow>>,
    pub rooms: Option<Vec<rooms::RoomRow>>,
    pub tasks: Option<Vec<tasks::TaskRow>>,
    pub team: Option<Vec<agents::TeamRow>>,
}
impl Rows {
    pub fn tabs(&self) -> Vec<Tab> {
        [
            (Tab::Agents, self.agents.is_some()),
            (Tab::Rooms, self.rooms.is_some()),
            (Tab::Tasks, self.tasks.is_some()),
            (Tab::Team, self.team.is_some()),
        ]
        .into_iter()
        .filter_map(|(tab, exists)| exists.then_some(tab))
        .collect()
    }
    pub fn len(&self, tab: Tab) -> usize {
        match tab {
            Tab::Agents => self.agents.as_ref().map_or(0, Vec::len),
            Tab::Rooms => self.rooms.as_ref().map_or(0, Vec::len),
            Tab::Tasks => self.tasks.as_ref().map_or(0, Vec::len),
            Tab::Team => self.team.as_ref().map_or(0, Vec::len),
        }
    }
    pub fn identity(&self, tab: Tab, at: usize) -> String {
        match tab {
            Tab::Agents => self
                .agents
                .as_ref()
                .and_then(|r| r.get(at))
                .map(|r| r.summary.id.to_string()),
            Tab::Rooms => self
                .rooms
                .as_ref()
                .and_then(|r| r.get(at))
                .map(|r| r.name.clone()),
            Tab::Tasks => self
                .tasks
                .as_ref()
                .and_then(|r| r.get(at))
                .map(|r| r.id.to_string()),
            Tab::Team => self
                .team
                .as_ref()
                .and_then(|r| r.get(at))
                .map(|r| r.role.clone()),
        }
        .unwrap_or_default()
    }
}
pub async fn read(host: &HostHandle, session: &SessionId) -> Result<Rows, KernelError> {
    let mut rows = Rows::default();
    if let Some(service) = agents::find(host) {
        rows.agents = Some(
            service
                .roster(agents::Session {
                    session: session.clone(),
                })
                .await
                .map_err(crate::error)?,
        );
        rows.team = Some(
            service
                .team(agents::Session {
                    session: session.clone(),
                })
                .await
                .map_err(crate::error)?,
        );
    }
    if let Some(service) = rooms::find(host) {
        rows.rooms = Some(
            service
                .rooms(rooms::Session {
                    session: session.clone(),
                })
                .await
                .map_err(crate::error)?,
        );
    }
    if let Some(service) = tasks::find(host) {
        rows.tasks = Some(
            service
                .board(tasks::Session {
                    session: session.clone(),
                })
                .await
                .map_err(crate::error)?,
        );
    }
    Ok(rows)
}
pub fn clamp(panel: &mut Panel, rows: &Rows) {
    if !rows.tabs().contains(&panel.tab) {
        panel.tab = rows.tabs().first().copied().unwrap_or_default();
    }
    panel.selected = panel.selected.min(rows.len(panel.tab).saturating_sub(1));
}
