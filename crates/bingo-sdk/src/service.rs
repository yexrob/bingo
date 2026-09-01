//! One service, met across a process line (ADR-0031).
//!
//! In process, a service is met by type: its owner contributes a live object
//! under a key and a consumer downcasts it to the trait both of them import.
//! A `TypeId` cannot cross a process, so a service that wants to reach one is
//! met the way the tool contract already is — a string key, a method name and
//! JSON — and [`WireService`] is the whole of that face. It is the only trait
//! this design mints: what a particular service *means* lives in its own api
//! crate, never here, because the kernel keeps no feature nouns.
//!
//! Crossing is the owner's choice. A service registers its wire face beside
//! its typed handle, and one with no wire face does not exist to a process at
//! all (ADR-0031 §3).

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde_json::Value;

/// Why a call did not answer, in words: the method a service does not speak,
/// the process that is not running, the deadline that ran out. There is
/// nothing here to branch on — a consumer that needs kinds is talking to a
/// typed trait in an api crate, not to this face.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ServiceError(pub String);

/// A service as JSON: one method name, what it takes, what it gives.
///
/// The implementation is either an adapter over a typed handle — mechanical,
/// written by whoever owns the service — or a bridge's proxy for a service
/// that lives in another process. A caller cannot tell which.
#[async_trait]
pub trait WireService: Send + Sync {
    /// A method this service does not speak is refused with the set it does
    /// (ADR-0031 §5); the params and the answer are whatever the service's
    /// declared schema says they are.
    async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError>;
}

/// What a service that lives in another process looks like from in here.
///
/// One concrete type, so a consumer reaches an external service through the
/// one lookup — `host.service::<ServiceHandle>(key)` — and calls it by
/// method. N external services are N of these; they differ by the process
/// behind them, never by type.
pub struct ServiceHandle(Arc<dyn WireService>);

impl ServiceHandle {
    pub fn new(wire: Arc<dyn WireService>) -> Self {
        Self(wire)
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        self.0.call(method, params).await
    }
}

impl std::fmt::Debug for ServiceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceHandle")
    }
}

/// One service, as whoever keeps them keeps it: two faces of one live object.
pub struct Service {
    /// What `host.service::<T>(key)` downcasts to `T`.
    pub value: Arc<dyn Any + Send + Sync>,
    /// What a process reaches by `service/call`, when the owner opened one.
    pub wire: Option<Arc<dyn WireService>>,
}

/// Every service in this process, by key.
///
/// One entry per key holds both faces: a second map keyed by the same string
/// would be two answers to "what is `kv`". Entries arrive while plugins
/// register and — for a service an external process declares, which nothing
/// can know until its handshake answered — after that, which is why this is
/// locked rather than built once.
#[derive(Default)]
pub struct Services(Mutex<HashMap<String, Service>>);

impl Services {
    /// Both faces, from the plugin that owns them. A key that is taken stays
    /// its first owner's, and this says so.
    pub fn add(&self, key: String, service: Service) -> Result<(), String> {
        let mut services = self.held();
        if services.contains_key(&key) {
            return Err(format!("service {key} is already registered"));
        }
        services.insert(key, service);
        Ok(())
    }

    /// A service that needed I/O to exist (ADR-0031 §4). The wire face is the
    /// object; the typed face is the handle over it, so an in-process
    /// consumer reaches an external service through the one lookup.
    pub fn open(&self, key: &str, wire: Arc<dyn WireService>) -> Result<(), String> {
        let value = Arc::new(ServiceHandle::new(Arc::clone(&wire)));
        self.add(
            key.to_string(),
            Service {
                value,
                wire: Some(wire),
            },
        )
    }

    pub fn value(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.held()
            .get(key)
            .map(|service| Arc::clone(&service.value))
    }

    pub fn wire(&self, key: &str) -> Option<Arc<dyn WireService>> {
        self.held()
            .get(key)
            .and_then(|service| service.wire.clone())
    }

    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.held().keys().cloned().collect();
        keys.sort();
        keys
    }

    fn held(&self) -> MutexGuard<'_, HashMap<String, Service>> {
        self.0.lock().unwrap_or_else(|held| held.into_inner())
    }
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Services").field(&self.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Kv;

    #[async_trait]
    impl WireService for Kv {
        async fn call(&self, method: &str, _params: Value) -> Result<Value, ServiceError> {
            Ok(json!(method))
        }
    }

    fn typed() -> Arc<dyn Any + Send + Sync> {
        Arc::new("a live object".to_string())
    }

    /// The in-process lane, untouched: a service is a typed value under a key,
    /// and one that opened no wire face does not exist to a process.
    #[test]
    fn a_service_with_no_wire_face_is_still_a_service_in_process() {
        let services = Services::default();
        services
            .add(
                "kv".into(),
                Service {
                    value: typed(),
                    wire: None,
                },
            )
            .expect("a free key");
        assert!(services.value("kv").is_some_and(|v| v.is::<String>()));
        assert!(
            services.wire("kv").is_none(),
            "crossing is the owner's choice"
        );
    }

    /// One entry, two faces: an opened service answers both lookups, and the
    /// object behind them is the one that was opened.
    #[tokio::test]
    async fn an_opened_service_answers_by_type_and_by_wire() {
        let services = Services::default();
        services.open("kv", Arc::new(Kv)).expect("a free key");
        let handle = services
            .value("kv")
            .and_then(|value| value.downcast::<ServiceHandle>().ok())
            .expect("the one lookup finds the handle");
        assert_eq!(
            handle.call("get", Value::Null).await.expect("it answers"),
            json!("get")
        );
        let wire = services.wire("kv").expect("and a process finds the face");
        assert_eq!(
            wire.call("set", Value::Null).await.expect("it answers"),
            json!("set")
        );
        assert_eq!(services.keys(), ["kv"]);
    }

    #[test]
    fn a_key_that_is_taken_stays_its_first_owner_s() {
        let services = Services::default();
        services.open("kv", Arc::new(Kv)).expect("a free key");
        let why = services
            .add(
                "kv".into(),
                Service {
                    value: typed(),
                    wire: None,
                },
            )
            .expect_err("the second is refused");
        assert_eq!(why, "service kv is already registered");
        assert!(
            services.wire("kv").is_some(),
            "and the first is still the one"
        );
    }
}
