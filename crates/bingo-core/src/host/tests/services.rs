//! What the registry does with a service (ADR-0031 §1): one entry per key,
//! holding both faces of one live object. The typed face is the lane that has
//! always been there; the wire face is there only if its owner opened one, and
//! a service an external process declares arrives after every plugin has
//! registered, because nothing knows of it until its handshake answers.

use super::*;

/// A typed handle under a key, met by `TypeId`: the lane ADR-0031 leaves
/// exactly as it was, and a service that opened no wire face is not reachable
/// from another process at all.
#[tokio::test]
async fn a_service_is_a_typed_value_under_a_key_and_crosses_only_if_it_opened_a_face() {
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Service {
            key: "test.counter".into(),
            value: Arc::new(7u32),
            wire: None,
        }],
    )];
    let host = Host::build(plugins, HostConfig::new(env())).await.unwrap();
    let handle = host.handle();
    assert_eq!(handle.service::<u32>("test.counter").as_deref(), Some(&7));
    assert!(handle.service::<String>("test.counter").is_none());
    assert!(handle.service_wire("test.counter").is_none());
    assert!(handle.service::<u32>("test.absent").is_none());
}

/// A service an external process declares cannot be there when the plugins
/// register, so it is opened once its handshake has answered — and then both
/// faces are the one object (ADR-0031 §4).
#[tokio::test]
async fn a_service_opened_late_answers_by_key_and_by_wire() {
    struct Echo;

    #[async_trait]
    impl WireService for Echo {
        async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
            Ok(json!({ "method": method, "params": params }))
        }
    }

    let (host, _) = host_with(vec![]).await;
    let handle = host.handle();
    handle.open_service("test.kv", Arc::new(Echo)).unwrap();
    let service = handle
        .service::<ServiceHandle>("test.kv")
        .expect("the one lookup finds an external service");
    assert_eq!(
        service.call("get", json!({ "key": "k" })).await.unwrap(),
        json!({ "method": "get", "params": { "key": "k" } })
    );
    assert!(
        handle.service_wire("test.kv").is_some(),
        "and a process reaches the same object"
    );
    let taken = handle
        .open_service("test.kv", Arc::new(Echo))
        .expect_err("a key that is taken stays its first owner's");
    assert_eq!(taken.code, ErrorCode::InvalidInput);
    assert!(taken.message.contains("test.kv"), "{taken}");
}
