//! The few questions this gateway has for the operating system's process
//! table, and the one instruction it gives it.
//!
//! `libc` and `unsafe` stay banned (ADR-0020 §3), so each one is a subprocess
//! a shell would run: `kill -0` to ask whether a pid is there, `ps` to ask
//! what it is running, `kill -TERM` to ask it to leave. That is three process
//! spawns, which is why this is a trait — a test fakes the table instead of
//! filling the machine with processes to ask about.

/// A pid, asked about from outside.
pub trait Probe: std::fmt::Debug + Send + Sync {
    /// `kill -0`: is there a process with this id that we may signal?
    fn alive(&self, pid: u32) -> bool;

    /// `ps -o comm=`: the name of the program that pid is running, when it is
    /// running one. A pid is reused as soon as the number comes round again,
    /// so this is what keeps `stop` from signalling a stranger (M17 R-liveness).
    fn command(&self, pid: u32) -> Option<String>;

    /// `kill -TERM`: ask that process to end, gracefully.
    fn terminate(&self, pid: u32) -> Result<(), String>;
}

/// The real table.
#[derive(Debug, Default, Clone, Copy)]
pub struct Kill;

impl Probe for Kill {
    fn alive(&self, pid: u32) -> bool {
        status(&["-0", &pid.to_string()])
    }

    fn command(&self, pid: u32) -> Option<String> {
        let out = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Some(name).filter(|name| !name.is_empty())
    }

    fn terminate(&self, pid: u32) -> Result<(), String> {
        match status(&["-TERM", &pid.to_string()]) {
            true => Ok(()),
            false => Err(format!("kill -TERM {pid} was refused")),
        }
    }
}

/// `kill` with these arguments, and whether it was happy. A `kill` that could
/// not be run at all reads as "no", which is the safe answer for every caller:
/// nothing is believed alive and nothing is believed signalled.
fn status(arguments: &[&str]) -> bool {
    std::process::Command::new("kill")
        .args(arguments)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether a pid is a bingo, as far as the process table will say. A `ps` that
/// answers nothing is not evidence against — it is no evidence at all, and a
/// caller that must not signal a stranger treats it as a stranger.
pub fn is_bingo(probe: &dyn Probe, pid: u32) -> bool {
    probe
        .command(pid)
        .is_some_and(|command| command.contains("bingo"))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// A process table a test writes down: what is alive, what it is running,
    /// and what was signalled.
    #[derive(Debug, Default)]
    pub struct Fake {
        running: BTreeMap<u32, String>,
        pub signalled: Mutex<Vec<u32>>,
    }

    impl Fake {
        /// A table in which each of these pids runs that program.
        pub fn of(running: &[(u32, &str)]) -> Self {
            Self {
                running: running
                    .iter()
                    .map(|(pid, name)| (*pid, (*name).to_string()))
                    .collect(),
                signalled: Mutex::new(Vec::new()),
            }
        }

        pub fn empty() -> Self {
            Self::of(&[])
        }

        pub fn signals(&self) -> Vec<u32> {
            self.signalled
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .clone()
        }
    }

    impl Probe for Fake {
        fn alive(&self, pid: u32) -> bool {
            self.running.contains_key(&pid)
        }

        fn command(&self, pid: u32) -> Option<String> {
            self.running.get(&pid).cloned()
        }

        fn terminate(&self, pid: u32) -> Result<(), String> {
            if !self.alive(pid) {
                return Err(format!("kill -TERM {pid} was refused"));
            }
            self.signalled
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .push(pid);
            Ok(())
        }
    }

    /// A pid that was real and is not any more: the surest dead pid a test can
    /// name, because a number picked out of the air might be somebody.
    fn reaped() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("`true` runs");
        let pid = child.id();
        child.wait().expect("it exits at once");
        pid
    }

    #[test]
    fn the_real_table_knows_this_process_is_here_and_a_reaped_one_is_not() {
        let probe = Kill;
        assert!(probe.alive(std::process::id()), "this test is running");
        assert!(!probe.alive(reaped()), "a waited-for child is gone");
    }

    #[test]
    fn the_real_table_names_the_program_a_live_pid_is_running() {
        let probe = Kill;
        let name = probe
            .command(std::process::id())
            .expect("ps names this process");
        assert!(
            name.contains("gateway") || name.contains("cli") || name.contains("bingo"),
            "the test binary's own name, whatever cargo called it: {name}"
        );
        assert_eq!(probe.command(reaped()), None);
    }

    #[test]
    fn a_stranger_is_not_a_bingo_and_a_missing_answer_is_not_either() {
        let table = Fake::of(&[(1, "launchd"), (2, "bingo"), (3, "bingo-gateway")]);
        assert!(!is_bingo(&table, 1));
        assert!(is_bingo(&table, 2));
        assert!(is_bingo(&table, 3), "argv[0] may carry a suffix");
        assert!(!is_bingo(&table, 4), "nothing there is not a bingo");
    }

    #[test]
    fn the_fake_refuses_to_signal_what_is_not_running() {
        let table = Fake::of(&[(7, "bingo")]);
        table.terminate(7).expect("7 is there");
        table.terminate(8).expect_err("8 is not");
        assert_eq!(table.signals(), vec![7]);
    }
}
