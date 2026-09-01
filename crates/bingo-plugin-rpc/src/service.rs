//! One service across the pipe, in both directions (ADR-0031 §4).
//!
//! Out: a service a plugin process declared, wearing the sdk's own
//! [`WireService`] so the registry holds it beside every in-process one — its
//! `call` is a `service/call` request and the answer is the service's own
//! JSON. In: a `service/call` a process sent the host, routed by key through
//! the registry. External ↔ external follows from those two: in one door, out
//! the other, and no pipe between the processes.
//!
//! The handle a service is published under outlives the process behind it. It
//! asks its bridge for the live connection on every call, so a plugin that
//! died and came back serves the same key without being published twice.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::{HostHandle, ServiceError, WireService};
use serde_json::Value;

use crate::bridge::Bridge;
use crate::codec::{INTERNAL_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, RpcError};
use crate::connection::Connection;
use crate::deadline;
use crate::wire::{ServiceCallParams, ServiceCallResult, ServiceSpec, host, name};

/// A service one plugin process declared, as the one face the registry holds.
/// N remote services are N of these; they differ by the declaration they were
/// built from, never by type.
pub struct RemoteService {
    /// The plugin's name, which every error is reported under; the service's
    /// key is the one another plugin calls it by.
    plugin: String,
    key: String,
    spec: ServiceSpec,
    /// Weak: the registry keeps this entry for the life of the host, and a
    /// service whose bridge is gone refuses in words rather than holding it.
    bridge: Weak<Bridge>,
}

impl RemoteService {
    pub fn new(plugin: &str, key: &str, spec: ServiceSpec, bridge: Weak<Bridge>) -> Self {
        Self {
            plugin: plugin.to_string(),
            key: key.to_string(),
            spec,
            bridge,
        }
    }

    /// A method the declaration never named does not cross: the process would
    /// refuse it anyway, and the set it does speak is what the caller needs to
    /// see (ADR-0031 §5). A service that named no method speaks none.
    fn speaks(&self, method: &str) -> Result<(), ServiceError> {
        if self.spec.methods.contains_key(method) {
            return Ok(());
        }
        Err(self.failed(format!(
            "the service {} does not speak {method}; it speaks {}",
            self.key,
            self.spoken()
        )))
    }

    fn spoken(&self) -> String {
        if self.spec.methods.is_empty() {
            return "nothing".to_string();
        }
        self.spec
            .methods
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The answer, or why there is none. A process that is slow is bounded by
    /// the one deadline: two services calling each other in a ring hang until
    /// it runs out, which is the floor this design has instead of a cycle
    /// detector.
    async fn ask(&self, method: &str, input: Value) -> Result<Value, ServiceError> {
        let connection = self.live().await?;
        let params = ServiceCallParams {
            key: self.key.clone(),
            method: method.to_string(),
            params: input,
        };
        let value = serde_json::to_value(params).map_err(|error| self.failed(error.to_string()))?;
        let answered = tokio::time::timeout(
            deadline::SERVICE,
            connection.request(name::SERVICE_CALL, value),
        )
        .await;
        match answered {
            Ok(Ok(value)) => serde_json::from_value::<ServiceCallResult>(value)
                .map(|answer| answer.result)
                .map_err(|error| self.failed(error.to_string())),
            Ok(Err(error)) => Err(self.failed(error.message)),
            Err(_) => Err(self.failed(format!(
                "the service {} said nothing within {}s",
                self.key,
                deadline::SERVICE.as_secs()
            ))),
        }
    }

    /// The pipe this call goes out on, asked for afresh: a plugin that died
    /// and was respawned answers on the connection it has now.
    async fn live(&self) -> Result<Arc<Connection>, ServiceError> {
        let bridge = self
            .bridge
            .upgrade()
            .ok_or_else(|| self.failed("the plugin is gone".into()))?;
        bridge
            .connection()
            .await
            .ok_or_else(|| self.failed("the plugin is not running".into()))
    }

    fn failed(&self, why: String) -> ServiceError {
        ServiceError(format!("{}: {why}", self.plugin))
    }
}

#[async_trait]
impl WireService for RemoteService {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        self.speaks(method)?;
        self.ask(method, params).await
    }
}

/// Who answers the one request a process may send. The connection knows that
/// method's name and nothing about what it means.
#[async_trait]
pub trait ServiceCalls: Send + Sync {
    async fn call(&self, params: Value) -> Result<Value, RpcError>;
}

/// The host as a router: a `service/call` a process sent, resolved by key
/// against the registry's wire faces (ADR-0031 §4). Two processes pair
/// through here — in one door, out the other — and nothing else in the host is
/// reachable from a process at all.
pub struct Hub {
    /// Who asked; a refusal is worth a line naming them.
    plugin: String,
    host: HostHandle,
    /// The host's own service, wearing the face bound to this connection
    /// (ADR-0033 §1). The registry holds the same doors under the same key,
    /// bound to the host itself; which face answers is what tells one
    /// process's running calls from another's, and a process must not be able
    /// to pick.
    doors: Arc<dyn WireService>,
}

impl Hub {
    pub fn new(plugin: &str, host: HostHandle, doors: Arc<dyn WireService>) -> Self {
        Self {
            plugin: plugin.to_string(),
            host,
            doors,
        }
    }

    /// The service's wire face, or why there is none: a key nobody holds, or
    /// one whose owner never opened a face. Crossing is the owner's choice
    /// (ADR-0031 §3), and what did not choose it does not exist out there.
    fn wire(&self, key: &str) -> Result<Arc<dyn WireService>, RpcError> {
        if key == host::KEY {
            return Ok(Arc::clone(&self.doors));
        }
        if let Some(wire) = self.host.service_wire(key) {
            return Ok(wire);
        }
        let why = match self.host.service_any(key) {
            Some(_) => format!("the service {key} has no wire face: it does not cross to a plugin"),
            None => format!("no service is registered under {key}"),
        };
        tracing::debug!(plugin = %self.plugin, key, %why, "a service call the host cannot route");
        Err(RpcError::new(METHOD_NOT_FOUND, why))
    }
}

#[async_trait]
impl ServiceCalls for Hub {
    async fn call(&self, params: Value) -> Result<Value, RpcError> {
        let params: ServiceCallParams = serde_json::from_value(params)
            .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))?;
        let wire = self.wire(&params.key)?;
        let result = wire
            .call(&params.method, params.params)
            .await
            .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string()))?;
        serde_json::to_value(ServiceCallResult { result })
            .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::testing::unanswering;

    fn declared(methods: &[&str]) -> ServiceSpec {
        ServiceSpec {
            methods: methods
                .iter()
                .map(|method| ((*method).to_string(), json!({ "type": "object" })))
                .collect(),
        }
    }

    /// A remote service over a process that is alive and says nothing.
    fn remote(spec: ServiceSpec) -> (Arc<Bridge>, RemoteService) {
        let bridge = Bridge::live("stub", unanswering());
        let service = RemoteService::new("stub", "kv", spec, Arc::downgrade(&bridge));
        (bridge, service)
    }

    /// The refusal that never crosses the pipe: what the caller is told is the
    /// set the declaration named, so a typo is answerable without a round trip.
    #[tokio::test]
    async fn a_method_the_declaration_never_named_is_refused_with_the_set_it_speaks() {
        let (_bridge, service) = remote(declared(&["get", "set"]));
        let why = service
            .call("drop", Value::Null)
            .await
            .expect_err("the service does not speak it")
            .to_string();
        assert_eq!(
            why,
            "stub: the service kv does not speak drop; it speaks get, set"
        );
    }

    #[tokio::test]
    async fn a_service_that_named_no_method_speaks_nothing() {
        let (_bridge, service) = remote(ServiceSpec::default());
        let why = service
            .call("get", Value::Null)
            .await
            .expect_err("there is nothing it speaks")
            .to_string();
        assert!(why.ends_with("it speaks nothing"), "{why}");
    }

    /// The floor under a wedged process, on a clock that does not tick: it is
    /// alive, it says nothing, and the caller is not held forever. Two
    /// services calling each other in a ring end here too — the deadline is
    /// this design's answer to a cycle, and there is no detector.
    #[tokio::test(start_paused = true)]
    async fn a_service_that_says_nothing_gives_up_at_the_deadline_and_names_the_plugin() {
        let (_bridge, service) = remote(declared(&["get"]));
        let why = service
            .call("get", json!({ "key": "k" }))
            .await
            .expect_err("a process that says nothing answers nothing")
            .to_string();
        assert_eq!(why, "stub: the service kv said nothing within 30s");
    }

    /// A bridge that is gone is not a panic and not a hang: the words say so.
    #[tokio::test]
    async fn a_service_whose_plugin_is_gone_refuses_in_words() {
        let (bridge, service) = remote(declared(&["get"]));
        drop(bridge);
        let why = service
            .call("get", Value::Null)
            .await
            .expect_err("there is no bridge behind it")
            .to_string();
        assert_eq!(why, "stub: the plugin is gone");
    }

    /// A hub with the host's own doors behind the reserved key, which is what
    /// a bridge builds; these tests are about the routing, not the doors.
    fn hub(host: HostHandle) -> Hub {
        let doors = crate::doors::Doors::new(Arc::new(crate::notice::Notices::default()));
        Hub::new("caller", host, doors.face(crate::doors::Caller::Host))
    }

    fn asked(key: &str, method: &str) -> Value {
        serde_json::to_value(ServiceCallParams {
            key: key.into(),
            method: method.into(),
            params: Value::Null,
        })
        .expect("a call serialises")
    }

    #[tokio::test]
    async fn a_key_nobody_holds_is_refused_in_words() {
        let host = bingo_sdk::testing::ServiceHost::handle();
        let error = hub(host)
            .call(asked("kv", "get"))
            .await
            .expect_err("there is no such service");
        assert_eq!(error.code, METHOD_NOT_FOUND);
        assert_eq!(error.message, "no service is registered under kv");
    }

    /// Crossing is the owner's choice (ADR-0031 §3): an in-process service
    /// that opened no wire face does not exist to a process, and the refusal
    /// says which of the two it is.
    #[tokio::test]
    async fn a_service_with_no_wire_face_does_not_exist_to_a_process() {
        let host = bingo_sdk::testing::ServiceHost::holding("kv", Arc::new(7u32));
        let error = hub(host)
            .call(asked("kv", "get"))
            .await
            .expect_err("it kept its wire face to itself");
        assert_eq!(
            error.message,
            "the service kv has no wire face: it does not cross to a plugin"
        );
    }

    /// The consumer side, answering: the host hands the call to the wire face
    /// the key names, and the answer goes back in the shape the wire pins.
    #[tokio::test]
    async fn a_call_the_host_can_route_comes_back_as_the_service_answered() {
        struct Kv;

        #[async_trait]
        impl WireService for Kv {
            async fn call(&self, method: &str, _: Value) -> Result<Value, ServiceError> {
                Ok(json!({ "did": method }))
            }
        }

        let host = bingo_sdk::testing::ServiceHost::handle();
        host.open_service("kv", Arc::new(Kv)).expect("a free key");
        let answered = hub(host)
            .call(asked("kv", "get"))
            .await
            .expect("the host routed it");
        assert_eq!(answered, json!({ "result": { "did": "get" } }));
    }

    #[tokio::test]
    async fn a_line_that_is_not_a_service_call_is_refused_as_bad_params() {
        let host = bingo_sdk::testing::ServiceHost::handle();
        let error = hub(host)
            .call(json!({ "method": "get" }))
            .await
            .expect_err("a call names its service");
        assert_eq!(error.code, INVALID_PARAMS);
    }

    /// A declaration is a map, so what a bridge publishes is one service per
    /// key; this is the walk the bridge does, kept honest.
    #[test]
    fn a_declaration_names_one_service_per_key() {
        let declared: BTreeMap<String, ServiceSpec> =
            serde_json::from_value(json!({ "kv": { "methods": { "get": {} } } }))
                .expect("a declaration");
        assert_eq!(declared.keys().cloned().collect::<Vec<_>>(), ["kv"]);
    }
}
