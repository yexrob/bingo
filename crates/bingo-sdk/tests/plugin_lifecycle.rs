#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bingo_sdk::{Contribution, Env, HostHandle, KernelError, PluginLifecycle, Registrar};

struct Cleanup;

#[async_trait::async_trait]
impl PluginLifecycle for Cleanup {
    async fn disabled(&self, _: &HostHandle) -> Result<(), KernelError> {
        Ok(())
    }
}

#[test]
fn lifecycle_registration_keeps_one_owner_scoped_callback() {
    let home = tempfile::tempdir().unwrap();
    let mut registrar = Registrar::new(
        "fixture",
        serde_json::json!({}),
        Env::rooted(home.path()),
        Default::default(),
    );
    let cleanup: Arc<dyn PluginLifecycle> = Arc::new(Cleanup);
    registrar.add(Contribution::Lifecycle(cleanup.clone()));
    let contributions = registrar.into_contributions();
    assert_eq!(contributions.len(), 1);
    let Contribution::Lifecycle(registered) = &contributions[0] else {
        panic!("a lifecycle contribution retains its callback");
    };
    assert!(Arc::ptr_eq(registered, &cleanup));
    assert_eq!(format!("{:?}", contributions[0]), "Lifecycle");
}
