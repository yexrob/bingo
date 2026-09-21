use super::*;
use bingo_sdk::testing::NoHost;
use std::time::Duration;

fn schedules(home: &tempfile::TempDir) -> Arc<Schedules> {
    Arc::new(Schedules::new(home.path()))
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the scheduler changes state");
}

#[test]
fn a_process_that_has_not_started_holds_nothing() {
    let home = tempfile::tempdir().expect("a temp home");
    let schedules = schedules(&home);
    assert!(!schedules.held());
    assert!(schedules.holder().contains("no runner holds"));
    assert_eq!(schedules.store().dir(), home.path().join("schedules"));
}

#[tokio::test]
async fn an_existing_standby_takes_over_after_the_owner_stops() {
    let home = tempfile::tempdir().expect("a temp home");
    let first = schedules(&home);
    first.start(NoHost::handle());
    assert!(first.held());
    assert_eq!(first.holder(), "held by this process");

    let second = schedules(&home);
    second.start(NoHost::handle());
    assert!(!second.held(), "one runner per store");
    assert!(second.holder().starts_with("standby"));
    assert!(!second.holder().contains("remove it"));

    first.stop().await;
    assert!(!first.held());
    until(|| second.held()).await;
    assert_eq!(second.holder(), "held by this process");
    second.stop().await;
    assert!(first.store().dir().join("runner.lock").exists());
}

#[tokio::test]
async fn a_cancelled_standby_does_not_take_over() {
    let home = tempfile::tempdir().expect("a temp home");
    let first = schedules(&home);
    first.start(NoHost::handle());
    let second = schedules(&home);
    second.start(NoHost::handle());
    second.stop().await;
    first.stop().await;
    second.start(NoHost::handle());
    assert!(
        !second.held(),
        "a stopped plugin cannot restart its cancelled loop"
    );
    Claim::take(first.store().dir()).expect("no cancelled runner holds the file");
}

#[tokio::test]
async fn an_aborted_task_releases_ownership_without_a_stale_running_flag() {
    let home = tempfile::tempdir().expect("a temp home");
    let schedules = schedules(&home);
    schedules.start(NoHost::handle());
    let running = schedules
        .running
        .lock()
        .expect("the task")
        .take()
        .expect("started");
    running.abort();
    assert!(running.await.expect_err("aborted").is_cancelled());
    assert!(!schedules.held());
    Claim::take(schedules.store().dir()).expect("the task no longer owns the file");
    schedules.stop().await;
}

#[tokio::test]
async fn storage_errors_are_reported_and_retried_after_the_store_is_repaired() {
    let home = tempfile::tempdir().expect("a temp home");
    let schedules = schedules(&home);
    std::fs::write(schedules.store().dir(), "not a directory").expect("an invalid store");
    schedules.start(NoHost::handle());
    assert!(!schedules.held());
    assert!(
        schedules.trouble().is_some(),
        "the real storage error is visible"
    );
    assert!(!schedules.holder().contains("another runner"));
    std::fs::remove_file(schedules.store().dir()).expect("repair the fixture");
    until(|| schedules.held()).await;
    assert!(
        schedules.trouble().is_none(),
        "a recovered acquisition clears its failure"
    );
    schedules.stop().await;
}

#[tokio::test]
async fn shutdown_keeps_ownership_until_in_flight_dispatch_finishes() {
    use crate::entry::tests::entry;
    use crate::tests::Fixture;

    let fixture = Fixture::new();
    fixture.host.hold_opens();
    fixture.store().save(&entry()).expect("an overdue entry");
    fixture.schedules.start(fixture.handle());
    fixture.host.wait_for_open().await;
    let schedules = fixture.schedules.clone();
    let stopping = tokio::spawn(async move { schedules.stop().await });
    until(|| fixture.schedules.cancel.is_cancelled()).await;
    assert!(!stopping.is_finished(), "stop joins the in-flight dispatch");
    assert!(
        fixture.schedules.held(),
        "the old runner is still dispatching"
    );
    assert!(
        Claim::take(&fixture.dir()).is_err(),
        "a successor cannot overlap it"
    );
    fixture.host.release_opens();
    stopping.await.expect("shutdown finishes");
    assert!(!fixture.schedules.held());
    Claim::take(&fixture.dir()).expect("the successor may now take ownership");
}
