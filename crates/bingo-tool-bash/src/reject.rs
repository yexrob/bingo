//! What the tool refuses before it spawns anything.
//!
//! The child gets `/dev/null` on stdin and a pipe on stdout, so a program that
//! wants a terminal either garbles the transcript (full-screen monitors,
//! editors), reaches around the pipes for `/dev/tty` (`sudo`, `ssh`, pagers), or
//! exits at once with nothing to say (a bare REPL). None of them can be driven
//! from here, so they are answered with a reason and an alternative instead of a
//! spawn. A second table catches commands that never end on their own.
//!
//! Both tables are pure: no filesystem, no environment, no process. They read
//! the command as words, so a compound command is judged by its first word — the
//! permission gate is what splits `&&` and `|`.

/// A command as the tables see it: the leading `VAR=value` assignments, the
/// base program's name, and everything after it.
struct Cmd<'a> {
    assignments: Vec<&'a str>,
    name: &'a str,
    rest: &'a [&'a str],
}

/// One row of the interactive table: a reason when it refuses, nothing when it
/// has no opinion.
type Rule = fn(&Cmd) -> Option<String>;

const RULES: &[Rule] = &[
    monitor,
    full_screen,
    pager,
    debugger,
    repl,
    db_client,
    ssh,
    container,
    tmux,
];

/// Why this command needs a terminal, if it does.
pub fn interactive_reason(command: &str) -> Option<String> {
    let words = tokenise(command);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match unwrap(&words) {
        Unwrapped::Nothing => None,
        Unwrapped::Rejected(reason) => Some(reason),
        Unwrapped::Command(cmd) => RULES.iter().find_map(|rule| rule(&cmd)),
    }
}

/// Why this command never ends on its own, if it does not.
///
/// Only the two commands that truly never exit are refused: `watch`, and
/// `tail` asked to follow. The old harness also refused every shell loop and
/// every `tail`, which turned `for f in *.rs` and `tail -n 20 app.log` into
/// friction; a loop that hangs is bounded by the timeout like anything else.
pub fn periodic_reason(command: &str) -> Option<String> {
    let words = tokenise(command);
    let what = match words.first().map(String::as_str)? {
        "watch" => "`watch` repeats until something stops it",
        "tail" if follows(&words[1..]) => "`tail -f` follows its file until something stops it",
        _ => return None,
    };
    Some(format!(
        "{what}, and this tool waits for the command to exit; rejected. Pass `timeout` (milliseconds) to bound the run"
    ))
}

/// `-f`, `-F`, `--follow[=…]`, or a short-flag cluster holding `f`/`F`.
fn follows(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg.starts_with("--follow")
            || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains(['f', 'F']))
    })
}

/// Shell words, quotes honoured. An unbalanced quote is the shell's problem, not
/// the table's: fall back to whitespace so the base command is still judged.
fn tokenise(command: &str) -> Vec<String> {
    shlex::split(command)
        .unwrap_or_else(|| command.split_whitespace().map(str::to_string).collect())
}

/// What is left once the assignments and the wrapper commands are stepped over.
enum Unwrapped<'a> {
    /// Wrappers and assignments only: there is no program to judge.
    Nothing,
    /// A wrapper is the refusal on its own.
    Rejected(String),
    Command(Cmd<'a>),
}

/// Commands that stand in front of the real one. `sudo` and `doas` have their
/// own flag grammar and can refuse on their own; the rest are stepped over.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "command", "exec", "time", "xargs",
];

fn unwrap<'a>(words: &'a [&'a str]) -> Unwrapped<'a> {
    let mut assignments = Vec::new();
    let mut i = 0;
    while let Some(word) = words.get(i).copied() {
        if is_assignment(word) {
            assignments.push(word);
            i += 1;
            continue;
        }
        let name = program(word);
        if !WRAPPERS.contains(&name) {
            return Unwrapped::Command(Cmd {
                assignments,
                name,
                rest: &words[i + 1..],
            });
        }
        i += 1;
        if matches!(name, "sudo" | "doas")
            && let Some(reason) = sudo(name, words, &mut i)
        {
            return Unwrapped::Rejected(reason);
        }
    }
    Unwrapped::Nothing
}

/// `sudo` flags that take a separate value, which is not a command.
const SUDO_VALUED: &[&str] = &[
    "-u", "-g", "-C", "-p", "-D", "-R", "-T", "-r", "-t", "-U", "-S", "-P", "-h",
];

/// Step over `sudo`'s own flags, and say whether `sudo` itself is the refusal:
/// it asks for a password on `/dev/tty`, which this shell does not have, unless
/// the call says not to prompt. `-i`/`-s` are an interactive login shell.
fn sudo(name: &str, words: &[&str], i: &mut usize) -> Option<String> {
    let mut passwordless = false;
    let mut clears_timestamp = false;
    while let Some(flag) = words.get(*i).copied().filter(|w| w.starts_with('-')) {
        *i += 1;
        if matches!(flag, "-i" | "-s") {
            return Some(format!(
                "`{name} {flag}` opens an interactive login shell, which needs a terminal; rejected"
            ));
        }
        passwordless |= matches!(flag, "-n" | "--non-interactive" | "-V" | "--version");
        clears_timestamp |= matches!(flag, "-k" | "-K");
        if SUDO_VALUED.contains(&flag) && *i < words.len() {
            *i += 1;
        }
    }
    let bare = *i >= words.len();
    let never_prompts = passwordless || (clears_timestamp && bare);
    (!never_prompts).then(|| {
        format!(
            "{name} prompts for a password on the terminal, which this shell does not have; rejected. Pass `-n` so it fails instead of prompting"
        )
    })
}

/// A leading `NAME=value`, the shell's own way of setting one variable for one
/// command. `--pager=cat` is an argument, not an assignment.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The program a word names, without its directory.
fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

const MONITORS: &[&str] = &[
    "top", "htop", "btop", "bpytop", "bashtop", "btm", "nmon", "glances", "s-tui", "gtop", "vtop",
    "ktop", "ctop", "ytop",
];

/// Full-screen system monitors. `-b`/`--batch` prints one snapshot and exits.
fn monitor(cmd: &Cmd) -> Option<String> {
    if !MONITORS.contains(&cmd.name) {
        return None;
    }
    let batch = cmd
        .rest
        .iter()
        .any(|a| matches!(*a, "-b" | "-batch" | "--batch"));
    let name = cmd.name;
    (!batch).then(|| {
        format!(
            "{name} is a full-screen monitor and needs a terminal; rejected. `{name} -b -n 1` prints one snapshot"
        )
    })
}

const EDITORS: &[&str] = &[
    "vim", "vi", "nvim", "nano", "emacs", "micro", "pico", "mg", "ed", "ex", "kak", "kakoune",
    "helix", "hx", "ne", "zile", "joe",
];
const FILE_MANAGERS: &[&str] = &[
    "ranger",
    "lf",
    "yazi",
    "joshuto",
    "mc",
    "midnight-commander",
];
const TUIS: &[&str] = &[
    "lazygit",
    "tig",
    "lazydocker",
    "k9s",
    "kdash",
    "screen",
    "fzf",
];

/// Programs that paint the whole screen and read keys.
fn full_screen(cmd: &Cmd) -> Option<String> {
    let what = if EDITORS.contains(&cmd.name) {
        "an interactive editor"
    } else if FILE_MANAGERS.contains(&cmd.name) {
        "an interactive file manager"
    } else if TUIS.contains(&cmd.name) {
        "an interactive TUI program"
    } else {
        return None;
    };
    Some(format!(
        "{} is {what} and needs a terminal; rejected",
        cmd.name
    ))
}

const PAGERS: &[&str] = &["less", "more", "most", "man"];

/// Pagers wait for keys on `/dev/tty` even when their output is a pipe. A
/// `PAGER`-style assignment or an explicit pager flag says the caller has
/// already dealt with that.
fn pager(cmd: &Cmd) -> Option<String> {
    if !PAGERS.contains(&cmd.name) {
        return None;
    }
    let overridden = cmd
        .assignments
        .iter()
        .any(|a| a.split_once('=').is_some_and(|(k, _)| k.ends_with("PAGER")))
        || cmd.rest.iter().any(|a| {
            matches!(*a, "-P" | "--no-pager" | "-F" | "--quit-if-one-screen")
                || a.starts_with("--pager=")
        });
    let name = cmd.name;
    (!overridden).then(|| {
        format!(
            "{name} pages its output on the terminal; rejected. Pipe the command into `cat`, or set a pager (`PAGER=cat {name} …`)"
        )
    })
}

/// `gdb` without `-batch` is an interactive debugger.
fn debugger(cmd: &Cmd) -> Option<String> {
    let batch = cmd.rest.iter().any(|a| matches!(*a, "-batch" | "--batch"));
    (cmd.name == "gdb" && !batch).then(|| {
        "gdb is an interactive debugger and needs a terminal; rejected. `gdb -batch -ex …` runs a script"
            .to_string()
    })
}

const REPLS: &[&str] = &[
    "bash",
    "sh",
    "zsh",
    "fish",
    "ksh",
    "dash",
    "elvish",
    "xonsh",
    "python",
    "python2",
    "python3",
    "ipython",
    "pypy",
    "node",
    "deno",
    "bun",
    "ruby",
    "irb",
    "perl",
    "php",
    "lua",
    "luajit",
    "bc",
    "dc",
    "sbcl",
    "ghci",
    "powershell",
    "pwsh",
];

/// A shell or language REPL with no arguments has nothing to read: stdin is
/// closed, so it exits at once. With arguments it is an ordinary command.
fn repl(cmd: &Cmd) -> Option<String> {
    let name = cmd.name;
    (REPLS.contains(&name) && cmd.rest.is_empty()).then(|| {
        format!(
            "{name} with no arguments is an interactive REPL and needs a terminal; rejected. `{name} -c '…'` or a script argument runs fine"
        )
    })
}

const DB_CLIENTS: &[&str] = &["sqlite3", "psql", "mysql", "mongosh", "redis-cli"];

/// Database clients drop into their own prompt unless the call already carries
/// what to run: an execution flag, a script, or redirected input.
fn db_client(cmd: &Cmd) -> Option<String> {
    if !DB_CLIENTS.contains(&cmd.name) {
        return None;
    }
    let has = |flags: &[&str]| cmd.rest.iter().any(|a| flags.contains(a));
    let stdin = cmd.rest.contains(&"<");
    let positional: Vec<&&str> = cmd.rest.iter().filter(|a| !a.starts_with('-')).collect();
    let prompts = match cmd.name {
        "sqlite3" | "psql" => {
            !has(&[
                "-c",
                "-f",
                "-l",
                "--command",
                "--file",
                "--list",
                "--version",
                "--help",
            ]) && !stdin
                && positional.len() <= 1
        }
        "mysql" => !has(&["-e", "--execute", "-f", "--force", "--version", "--help"]) && !stdin,
        "mongosh" => {
            !has(&["--eval", "--version", "--help"])
                && !stdin
                && !positional.iter().any(|a| a.ends_with(".js"))
        }
        _ => !has(&["--version", "--help"]) && positional.is_empty() && !stdin,
    };
    let name = cmd.name;
    prompts.then(|| {
        format!(
            "{name} opens an interactive client and needs a terminal; rejected. Pass the statement instead (`{name} -c '…'`)"
        )
    })
}

/// What an `ssh` argument list asks for.
struct SshShape {
    host: bool,
    remote_command: bool,
    /// `-N`/`-f`: no session on this terminal at all.
    detached: bool,
}

const SSH_VALUED: &[&str] = &[
    "-p", "-l", "-i", "-o", "-F", "-J", "-L", "-R", "-D", "-W", "-m", "-c", "-e", "-b", "-K", "-I",
    "-O", "-Q", "-S", "-w", "-E", "-G", "-g",
];

/// `ssh` with a host but no remote command opens a session, and `-t` demands a
/// terminal outright.
fn ssh(cmd: &Cmd) -> Option<String> {
    if cmd.name != "ssh" {
        return None;
    }
    let forced = cmd.rest.iter().any(|a| matches!(*a, "-t" | "-tt"));
    let shape = ssh_shape(cmd.rest);
    (forced || (shape.host && !shape.remote_command && !shape.detached)).then(|| {
        "ssh takes the terminal for a password prompt or a remote shell; rejected. Pass the remote command (`ssh host 'cmd'`)"
            .to_string()
    })
}

fn ssh_shape(rest: &[&str]) -> SshShape {
    let mut shape = SshShape {
        host: false,
        remote_command: false,
        detached: false,
    };
    let mut i = 0;
    while let Some(a) = rest.get(i).copied() {
        if SSH_VALUED.contains(&a) {
            i += 2;
            continue;
        }
        if matches!(a, "-N" | "-f") {
            shape.detached = true;
        } else if !a.starts_with('-') {
            if shape.host {
                shape.remote_command = true;
            } else {
                shape.host = true;
            }
        }
        i += 1;
    }
    shape
}

const CONTAINER_TOOLS: &[&str] = &["docker", "nerdctl", "podman", "kubectl", "docker-compose"];

/// `attach`, and `exec`/`run` with `-it`, open an interactive session.
fn container(cmd: &Cmd) -> Option<String> {
    if !CONTAINER_TOOLS.contains(&cmd.name) {
        return None;
    }
    let name = cmd.name;
    let sub = cmd.rest.first().copied().unwrap_or_default();
    if sub == "attach" {
        return Some(format!(
            "`{name} attach` is an interactive session and needs a terminal; rejected"
        ));
    }
    if !matches!(sub, "exec" | "run") {
        return None;
    }
    let interactive = cmd
        .rest
        .iter()
        .any(|a| matches!(*a, "-it" | "-ti" | "--interactive"))
        || (cmd.rest.contains(&"-i") && cmd.rest.contains(&"-t"));
    interactive.then(|| {
        format!(
            "`{name} {sub} -it` is an interactive session and needs a terminal; rejected. Drop `-t` to run it non-interactively"
        )
    })
}

/// A tmux session in the foreground needs a terminal; `tmux new -d` and the
/// scripted subcommands (`send-keys`, `capture-pane`) do not.
fn tmux(cmd: &Cmd) -> Option<String> {
    if cmd.name != "tmux" {
        return None;
    }
    let sub = cmd.rest.first().copied().unwrap_or_default();
    if matches!(sub, "attach" | "a" | "attach-session") {
        return Some(
            "`tmux attach` needs a terminal; rejected. `tmux new -d` starts a detached session"
                .to_string(),
        );
    }
    let foreground =
        cmd.rest.is_empty() || (matches!(sub, "new" | "new-session") && !cmd.rest.contains(&"-d"));
    foreground.then(|| {
        "a foreground tmux session needs a terminal; rejected. `tmux new -d` starts a detached session"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row: a command, and whether the table refuses it.
    const INTERACTIVE: &[(&str, bool)] = &[
        // Ordinary work is never touched.
        ("echo hi", false),
        ("cargo test --workspace", false),
        ("git status --short", false),
        ("", false),
        ("   ", false),
        // Wrappers are stepped over, and judge what they wrap.
        ("nohup vim", true),
        ("time htop", true),
        ("xargs vim", true),
        ("command vim", true),
        ("exec nano", true),
        ("env FOO=1 vim", true),
        ("env FOO=1 ls", false),
        ("nohup cargo build", false),
        ("/usr/bin/vim file.txt", true),
        // Leading assignments belong to the shell, not to the program.
        ("EDITOR=vi vim x", true),
        ("FOO=1 ls", false),
        // sudo prompts on the terminal unless it is told not to.
        ("sudo", true),
        ("sudo apt update", true),
        ("sudo -n apt update", false),
        ("sudo --non-interactive apt update", false),
        ("sudo -n vim", true),
        ("sudo -i", true),
        ("sudo -s", true),
        ("sudo -V", false),
        ("sudo -k", false),
        ("sudo -u root -n ls", false),
        ("doas ls", true),
        // Monitors, unless they take a snapshot.
        ("top", true),
        ("top -b -n 1", false),
        ("htop", true),
        ("btm --batch", false),
        // Editors, file managers, other TUIs.
        ("nano notes.txt", true),
        ("ranger", true),
        ("lazygit", true),
        ("fzf", true),
        // Pagers, unless the caller has dealt with the pager.
        ("less big.log", true),
        ("man ls", true),
        ("PAGER=cat man ls", false),
        ("man -P cat ls", false),
        ("less -F big.log", false),
        // gdb.
        ("gdb ./a.out", true),
        ("gdb -batch -ex run ./a.out", false),
        // REPLs are only a problem with nothing to run.
        ("python3", true),
        ("python3 script.py", false),
        ("node", true),
        ("node -e 'console.log(1)'", false),
        ("irb", true),
        ("bash", true),
        ("bash -c 'echo hi'", false),
        // Database clients.
        ("sqlite3 app.db", true),
        ("sqlite3 app.db 'select 1'", false),
        ("sqlite3 -c 'select 1' app.db", false),
        ("psql mydb", true),
        ("psql -c 'select 1' mydb", false),
        ("mysql -u root", true),
        ("mysql -e 'select 1'", false),
        ("mongosh", true),
        ("mongosh --eval 'db.x.find()'", false),
        ("redis-cli", true),
        ("redis-cli get key", false),
        // ssh.
        ("ssh host", true),
        ("ssh host uptime", false),
        ("ssh -p 22 host uptime", false),
        ("ssh -t host uptime", true),
        ("ssh -N -L 8080:localhost:80 host", false),
        // Containers.
        ("docker attach web", true),
        ("docker exec -it web bash", true),
        ("docker exec -i -t web bash", true),
        ("docker exec web ls", false),
        ("docker ps", false),
        ("kubectl exec -it pod -- sh", true),
        // tmux.
        ("tmux", true),
        ("tmux attach", true),
        ("tmux new -d -s work", false),
        ("tmux new -s work", true),
        ("tmux send-keys -t work 'ls' Enter", false),
    ];

    #[test]
    fn the_interactive_table_answers_every_row() {
        for (command, rejected) in INTERACTIVE {
            assert_eq!(
                interactive_reason(command).is_some(),
                *rejected,
                "{command:?} -> {:?}",
                interactive_reason(command)
            );
        }
    }

    #[test]
    fn every_refusal_says_why_and_names_it() {
        for (command, rejected) in INTERACTIVE {
            let Some(reason) = interactive_reason(command) else {
                continue;
            };
            assert!(rejected, "{command:?} was not meant to be refused");
            assert!(reason.contains("rejected"), "{command:?}: {reason}");
        }
    }

    #[test]
    fn a_refusal_offers_the_way_round_it() {
        let reason = interactive_reason("top").expect("top is refused");
        assert!(reason.contains("top -b -n 1"), "{reason}");
        let reason = interactive_reason("sudo apt update").expect("sudo is refused");
        assert!(reason.contains("-n"), "{reason}");
        let reason = interactive_reason("man ls").expect("man is refused");
        assert!(reason.contains("PAGER=cat"), "{reason}");
    }

    #[test]
    fn quoted_words_are_one_word() {
        assert!(interactive_reason("echo 'vim is nice'").is_none());
        assert!(interactive_reason("git commit -m 'run top'").is_none());
    }

    #[test]
    fn an_unbalanced_quote_still_reaches_the_base_command() {
        assert!(interactive_reason("vim 'unterminated").is_some());
    }

    /// Every row: a command, and whether it is refused as never-ending.
    const PERIODIC: &[(&str, bool)] = &[
        ("watch ls", true),
        ("watch -n 2 ls", true),
        ("tail -f app.log", true),
        ("tail -F app.log", true),
        ("tail --follow=name app.log", true),
        ("tail -fn 10 app.log", true),
        ("tail -n 20 app.log", false),
        ("while true; do echo hi; done", false),
        ("for i in 1 2 3; do echo $i; done", false),
        ("echo watch", false),
        ("ls | tail -f", false),
        ("cargo watch", false),
        ("", false),
    ];

    #[test]
    fn the_periodic_table_answers_every_row() {
        for (command, rejected) in PERIODIC {
            assert_eq!(
                periodic_reason(command).is_some(),
                *rejected,
                "{command:?} -> {:?}",
                periodic_reason(command)
            );
        }
    }

    #[test]
    fn a_periodic_refusal_points_at_the_timeout() {
        let reason = periodic_reason("watch ls").expect("watch is refused");
        assert!(reason.contains("timeout"), "{reason}");
    }
}
