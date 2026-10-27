//! Opt-in input transport only; the clipboard reader still owns the image.

#[cfg(windows)]
mod companion;
#[cfg(windows)]
static COMPANION: std::sync::Mutex<Option<companion::Companion>> = std::sync::Mutex::new(None);

#[cfg(windows)]
pub(super) fn start(set_title: impl FnOnce(&str) -> std::io::Result<()>) {
    use std::io::IsTerminal;
    let opened = start_when_wanted(
        std::env::var_os("BINGO_WINDOWS_IMAGE_PASTE").as_deref(),
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        || {
            companion::Companion::prepare().and_then(|helper| {
                let (marker, ready) = publish(helper, companion::Companion::begin)?;
                finish(&marker, companion::available(ready), set_title)
            })
        },
    );
    if matches!(opened, Some(Err(_))) {
        tracing::warn!(
            "experimental Windows image-paste companion unavailable; normal terminal paste unchanged"
        );
    }
}

#[cfg(windows)]
fn publish(
    mut helper: companion::Companion,
    begin: impl FnOnce(&mut companion::Companion) -> std::io::Result<std::sync::mpsc::Receiver<bool>>,
) -> std::io::Result<(String, std::sync::mpsc::Receiver<bool>)> {
    let mut slot = COMPANION.lock().unwrap_or_else(|error| error.into_inner());
    if !super::ENTERED.load(std::sync::atomic::Ordering::SeqCst) || slot.is_some() {
        return Err(std::io::Error::other("terminal no longer available"));
    }
    let marker = helper.marker().to_owned();
    let ready = begin(&mut helper)?;
    *slot = Some(helper);
    // Only spawn/publication is synchronized. Readiness waits and worker
    // destruction never hold this lock; restore can cancel startup immediately.
    Ok((marker, ready))
}

#[cfg(windows)]
fn finish(
    marker: &str,
    ready: bool,
    set_title: impl FnOnce(&str) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut slot = COMPANION.lock().unwrap_or_else(|error| error.into_inner());
    if !super::ENTERED.load(std::sync::atomic::Ordering::SeqCst)
        || slot.as_ref().is_none_or(|helper| helper.marker() != marker)
    {
        return Err(std::io::Error::other("terminal restored during startup"));
    }
    // The helper needs no title to become ready. Defer the marker until now,
    // serialized with restore's take, so no marker can be written after restore.
    let result = if ready {
        set_title(marker)
    } else {
        Err(std::io::Error::other("helper readiness unavailable"))
    };
    let failed = result.is_err().then(|| slot.take()).flatten();
    drop(slot);
    drop(failed);
    result
}

#[cfg(windows)]
pub(super) fn marked_title(text: &str) -> String {
    let helper = COMPANION.lock().unwrap_or_else(|error| error.into_inner());
    title(text, helper.as_ref().map(|helper| helper.marker()))
}

#[cfg(windows)]
pub(super) fn stop() {
    stop_slot(&COMPANION);
}

fn stop_slot<T>(slot: &std::sync::Mutex<Option<T>>) {
    let helper = slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    // Destruction may join a worker: never hold the global lock across it.
    drop(helper);
}

const READY: &[u8] = b"BINGO_IMAGE_PASTE_READY_V1\n";

fn wanted(value: Option<&std::ffi::OsStr>, stdin: bool, stdout: bool) -> bool {
    value == Some(std::ffi::OsStr::new("1")) && stdin && stdout
}

fn start_when_wanted<T>(
    value: Option<&std::ffi::OsStr>,
    stdin: bool,
    stdout: bool,
    open: impl FnOnce() -> T,
) -> Option<T> {
    wanted(value, stdin, stdout).then(open)
}

fn ready(bytes: &[u8]) -> bool {
    bytes == READY || bytes == b"BINGO_IMAGE_PASTE_READY_V1\r\n"
}

pub(super) fn title(text: &str, marker: Option<&str>) -> String {
    match marker {
        Some(marker) => format!("{text} {marker}"),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn only_explicit_opt_in_with_two_terminals_starts_a_companion() {
        for value in [None, Some(""), Some("0"), Some("true"), Some(" 1")] {
            assert!(!wanted(value.map(OsStr::new), true, true));
        }
        assert!(wanted(Some(OsStr::new("1")), true, true));
        assert!(!wanted(Some(OsStr::new("1")), false, true));
        assert!(!wanted(Some(OsStr::new("1")), true, false));
    }

    #[test]
    fn disabled_or_headless_start_does_no_extraction_launch_or_title_work() {
        for (value, stdin, stdout) in [
            (None, true, true),
            (Some("0"), true, true),
            (Some("1"), false, true),
            (Some("1"), true, false),
        ] {
            assert!(
                start_when_wanted(value.map(OsStr::new), stdin, stdout, || {
                    panic!("disabled startup must not prepare source, launch or set title")
                })
                .is_none()
            );
        }
    }

    #[test]
    fn restore_releases_a_poisoned_resource_before_returning_without_holding_its_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        struct Resource(Box<dyn Fn() + Send>);
        impl Drop for Resource {
            fn drop(&mut self) {
                (self.0)();
            }
        }
        let slot = Arc::new(Mutex::new(None));
        let weak = Arc::downgrade(&slot);
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = stopped.clone();
        *slot.lock().unwrap() = Some(Resource(Box::new(move || {
            let slot = weak.upgrade().unwrap();
            assert!(!matches!(
                slot.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            observed.store(true, Ordering::SeqCst);
        })));
        let _ = std::panic::catch_unwind(|| {
            let _lock = slot.lock().unwrap();
            panic!("simulate a panic while the slot is borrowed");
        });
        let other = slot.clone();
        std::thread::spawn(move || stop_slot(&other))
            .join()
            .unwrap();
        assert!(
            stopped.load(Ordering::SeqCst),
            "raw/title restoration may proceed only after stop returns"
        );
        stop_slot(&slot);
    }

    #[test]
    fn readiness_is_a_versioned_exact_line_not_arbitrary_stdout() {
        assert!(ready(READY));
        assert!(ready(b"BINGO_IMAGE_PASTE_READY_V1\r\n"));
        for line in [
            b"".as_slice(),
            b"BINGO_IMAGE_PASTE_READY_V1",
            b"BINGO_IMAGE_PASTE_READY_V2\n",
            b"warning BINGO_IMAGE_PASTE_READY_V1\n",
        ] {
            assert!(!ready(line));
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires scripts/test-windows-image-paste.ps1 isolated native console harness"]
    fn crossterm_ctrl_v_fixture() {
        use crate::test_support::{scene, solo, state};
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        use std::io::Write;
        use std::time::{Duration, Instant};
        struct Raw;
        impl Drop for Raw {
            fn drop(&mut self) {
                let _ = crossterm::terminal::disable_raw_mode();
            }
        }
        crossterm::terminal::enable_raw_mode().unwrap();
        let _raw = Raw;
        println!("BINGO_CROSSTERM_READING");
        std::io::stdout().flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !crossterm::event::poll(Duration::from_millis(100)).unwrap() {
                continue;
            }
            let Event::Key(key) = crossterm::event::read().unwrap() else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            assert_eq!(key.code, KeyCode::Char('v'));
            assert_eq!(key.modifiers, KeyModifiers::CONTROL);
            let (mut ui, now) = scene();
            assert_eq!(
                crate::input::on_key(&mut ui, &solo(&state()), key, now),
                vec![crate::effect::Effect::PasteImage]
            );
            return;
        }
        panic!("production console records never reached crossterm");
    }

    #[test]
    fn every_title_preserves_the_runs_marker_and_default_titles_are_unchanged() {
        let marker = "[bingo-image-paste-123-abc-0]";
        for text in ["bingo", "working · /repo", "needs you", "done"] {
            assert_eq!(title(text, None), text);
            assert_eq!(title(text, Some(marker)), format!("{text} {marker}"));
        }
    }
}
