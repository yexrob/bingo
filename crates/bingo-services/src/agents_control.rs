//! Typed agents-control operations and their process boundary.
use async_trait::async_trait;
use bingo_sdk::{Face, HostHandle, ServiceError, ServiceKey, SessionId, WireService};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
pub static KEY: ServiceKey<dyn AgentsControl> = ServiceKey::new("agents-control");
pub const CAPABILITY: &str = "service:agents-control";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRow {
    pub summary: bingo_sdk::SessionSummary,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TeamRow {
    pub role: String,
    pub agent: Option<String>,
    pub summary: Option<bingo_sdk::SessionSummary>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub session: SessionId,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Named {
    pub session: SessionId,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub session: SessionId,
    pub name: String,
    pub text: String,
}
#[async_trait]
pub trait AgentsControl: Send + Sync {
    async fn roster(&self, request: Session) -> Result<Vec<AgentRow>, ServiceError>;
    async fn team(&self, request: Session) -> Result<Vec<TeamRow>, ServiceError>;
    async fn stop(&self, request: Named) -> Result<String, ServiceError>;
    async fn dismiss(&self, request: Named) -> Result<String, ServiceError>;
    async fn message(&self, request: Message) -> Result<String, ServiceError>;
    async fn reopen(&self, request: Message) -> Result<String, ServiceError>;
}
pub fn find(host: &HostHandle) -> Option<Arc<dyn AgentsControl>> {
    host.service(&KEY).or_else(|| {
        host.service_wire(KEY.name())
            .map(|w| Arc::new(FromWire(w)) as Arc<dyn AgentsControl>)
    })
}
pub fn wire(owner: Arc<dyn AgentsControl>) -> Arc<dyn WireService> {
    Arc::new(ToWire(owner))
}
struct ToWire(Arc<dyn AgentsControl>);
struct FromWire(Arc<dyn WireService>);
fn error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError(e.to_string())
}
#[async_trait]
impl WireService for ToWire {
    fn face(&self) -> Option<Face> {
        let mut face = Face::new("agents-control")
            .method::<Session, Vec<AgentRow>>("roster", false)
            .method::<Session, Vec<TeamRow>>("team", false)
            .method::<Named, String>("stop", true)
            .method::<Named, String>("dismiss", true)
            .method::<Message, String>("message", true)
            .method::<Message, String>("reopen", true);
        for method in &mut face.methods {
            method.entry.pinned = &["/session"];
        }
        Some(face)
    }
    async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        match method {
            "roster" => serde_json::to_value(
                self.0
                    .roster(serde_json::from_value(params).map_err(error)?)
                    .await?,
            )
            .map_err(error),
            "team" => serde_json::to_value(
                self.0
                    .team(serde_json::from_value(params).map_err(error)?)
                    .await?,
            )
            .map_err(error),
            "stop" => serde_json::to_value(
                self.0
                    .stop(serde_json::from_value(params).map_err(error)?)
                    .await?,
            )
            .map_err(error),
            "dismiss" => serde_json::to_value(
                self.0
                    .dismiss(serde_json::from_value(params).map_err(error)?)
                    .await?,
            )
            .map_err(error),
            "message" => serde_json::to_value(
                self.0
                    .message(serde_json::from_value(params).map_err(error)?)
                    .await?,
            )
            .map_err(error),
            "reopen" => serde_json::to_value(
                self.0
                    .reopen(serde_json::from_value(params).map_err(error)?)
                    .await?,
            )
            .map_err(error),
            other => Err(error(format!("unknown method: {other}"))),
        }
    }
}
#[async_trait]
impl AgentsControl for FromWire {
    async fn roster(&self, request: Session) -> Result<Vec<AgentRow>, ServiceError> {
        let answer = self
            .0
            .call("roster", serde_json::to_value(request).map_err(error)?)
            .await?;
        serde_json::from_value(answer).map_err(error)
    }
    async fn team(&self, request: Session) -> Result<Vec<TeamRow>, ServiceError> {
        let answer = self
            .0
            .call("team", serde_json::to_value(request).map_err(error)?)
            .await?;
        serde_json::from_value(answer).map_err(error)
    }
    async fn stop(&self, request: Named) -> Result<String, ServiceError> {
        let answer = self
            .0
            .call("stop", serde_json::to_value(request).map_err(error)?)
            .await?;
        serde_json::from_value(answer).map_err(error)
    }
    async fn dismiss(&self, request: Named) -> Result<String, ServiceError> {
        let answer = self
            .0
            .call("dismiss", serde_json::to_value(request).map_err(error)?)
            .await?;
        serde_json::from_value(answer).map_err(error)
    }
    async fn message(&self, request: Message) -> Result<String, ServiceError> {
        let answer = self
            .0
            .call("message", serde_json::to_value(request).map_err(error)?)
            .await?;
        serde_json::from_value(answer).map_err(error)
    }
    async fn reopen(&self, request: Message) -> Result<String, ServiceError> {
        let answer = self
            .0
            .call("reopen", serde_json::to_value(request).map_err(error)?)
            .await?;
        serde_json::from_value(answer).map_err(error)
    }
}
#[cfg(test)]
mod tests;
