mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{write, write_executable, Harness};
use wt_sys::proc::{self, CommandRequest, ProcessOutput};

const SESSION_CONFIG: &str = "bin=['bin']\nports=['http']\n[env]\nAPP_PORT=\"{{ports.http}}\"\n";

struct PrivateTmux {
    harness: Harness,
    binary: PathBuf,
    socket: String,
}

impl PrivateTmux {
    fn new(agent: bool, attach: bool) -> Option<Self> {
        let binary = find_tmux()?;
        let harness = Harness::new();
        let socket = format!(
            "wt-test-{}-{}",
            std::process::id(),
            wt_sys::fsx::random_tree_id().ok()?
        );
        write_executable(
            &harness.shims.join("tmux"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nexec \"{}\" -L \"{}\" \"$@\"\n",
                harness.shim_state.join("real-tmux.log").display(),
                binary.display(),
                socket
            ),
        );
        let agent_path = harness.shims.join("session-agent");
        let agent_log = harness.shim_state.join("agent.log");
        write_executable(
            &agent_path,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"{}\"\nprintf '__AGENT_%s__\\n' \"$1\"\n",
                agent_log.display()
            ),
        );
        let agent_setting = if agent { "agent='probe'\n" } else { "" };
        write(
            &harness.home.join("config.toml"),
            &format!(
                "[session]\nbackend='tmux'\nattach={attach}\n{agent_setting}[shell]\nprogram='/bin/sh'\n[agents.probe]\nstart=['{}','start']\nresume=['{}','resume']\n",
                agent_path.display(),
                agent_path.display()
            ),
        );
        Some(Self {
            harness,
            binary,
            socket,
        })
    }

    fn tmux(&self, args: &[&str]) -> ProcessOutput {
        self.tmux_with_env(args, None)
    }

    fn tmux_with_env(&self, args: &[&str], wt_home: Option<&Path>) -> ProcessOutput {
        let mut request = CommandRequest::new(&self.binary);
        request.args = vec![OsString::from("-L"), OsString::from(&self.socket)];
        request.args.extend(proc::os_args(args));
        if let Some(home) = wt_home {
            request
                .env
                .insert("WT_HOME".to_owned(), home.to_string_lossy().into_owned());
        }
        proc::capture(&request, Duration::from_secs(2)).expect("run private tmux")
    }

    fn has_session(&self, session: &str) -> bool {
        self.tmux(&["has-session", "-t", session]).success()
    }

    fn wait_for_session(&self, session: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.has_session(session) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for private tmux session {session}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_pane(&self, session: &str, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = self.tmux(&["capture-pane", "-p", "-t", session]);
            let pane = String::from_utf8_lossy(&output.stdout).into_owned();
            if output.success() && pane.contains(marker) {
                return pane;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {marker:?} in {session}; pane:\n{pane}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send_line(&self, session: &str, line: &str) {
        assert!(self
            .tmux(&["send-keys", "-t", session, "-l", line])
            .success());
        assert!(self.tmux(&["send-keys", "-t", session, "Enter"]).success());
    }

    fn clients(&self, _session: &str) -> usize {
        let output = self.tmux(&["list-clients", "-F", "#{client_pid}"]);
        if output.success() {
            String::from_utf8_lossy(&output.stdout).lines().count()
        } else {
            0
        }
    }

    fn wait_for_client(&self, session: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.clients(session) == 0 {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a client attached to {session}; wrapper calls: {:?}",
                wt_sys::fsx::read_string(&self.harness.shim_state.join("real-tmux.log")).unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn agent_events(&self) -> Vec<String> {
        wt_sys::fsx::read_string(&self.harness.shim_state.join("agent.log"))
            .unwrap()
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn session_gets_resolved_home_even_when_server_captured_another_one() {
    let Some(private) = PrivateTmux::new(false, false) else {
        eprintln!("skipping private-tmux home test: tmux is not installed");
        return;
    };
    let wrong_home = private.harness.root.join("server-home");
    wt_sys::fsx::create_private_dir(&wrong_home).unwrap();
    assert!(private
        .tmux_with_env(&["new-session", "-d", "-s", "keeper"], Some(&wrong_home))
        .success());

    let observed = private.harness.root.join("observed-home");
    let agent = private.harness.shims.join("home-agent");
    write_executable(
        &agent,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$WT_HOME\" > \"{}\"\nexec /bin/sh -i\n",
            observed.display()
        ),
    );
    write(
        &private.harness.home.join("config.toml"),
        &format!(
            "[session]\nbackend='tmux'\nattach=false\n[agents.home]\nstart=['{}']\nresume=['{}']\n",
            agent.display(),
            agent.display()
        ),
    );
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);
    let opened = private
        .harness
        .json(&["open", "repo", "--agent", "home", "--no-attach"]);
    assert_eq!(opened["data"]["sessions"][0]["created"], true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !observed.exists() {
        assert!(Instant::now() < deadline, "agent never recorded WT_HOME");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&observed).unwrap().trim(),
        private.harness.home.to_string_lossy()
    );
    assert_ne!(private.harness.home, wrong_home);
    common::proof_capture(
        "E1",
        format!(
            "tmux server WT_HOME: {}\nsession WT_HOME: {}",
            wrong_home.display(),
            std::fs::read_to_string(&observed).unwrap().trim()
        )
        .replace(
            &std::fs::canonicalize(&private.harness.root)
                .unwrap_or_else(|_| private.harness.root.clone())
                .to_string_lossy()
                .to_string(),
            "<ROOT>",
        )
        .replace(
            &private.harness.root.to_string_lossy().to_string(),
            "<ROOT>",
        ),
    );
}

#[test]
fn open_reports_a_session_that_dies_during_startup() {
    let Some(private) = PrivateTmux::new(false, false) else {
        eprintln!("skipping private-tmux startup test: tmux is not installed");
        return;
    };
    let dead_shell = private.harness.shims.join("dead-shell");
    write_executable(
        &dead_shell,
        "#!/bin/sh\nprintf 'DEAD_SHELL_OUTPUT\\n'\nexit 9\n",
    );
    write(
        &private.harness.home.join("config.toml"),
        &format!(
            "[session]\nbackend='tmux'\nattach=false\n[shell]\nprogram='{}'\n",
            dead_shell.display()
        ),
    );
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);
    let output = private
        .harness
        .wt()
        .args(["open", "repo", "--no-attach", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "SESSION_CREATE_FAILED");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("startup observation window"), "{message}");
    assert!(message.contains("DEAD_SHELL_OUTPUT"), "{message}");
    let session = wt_core::session::name("repo", "canonical");
    assert!(!private.has_session(&session));
    common::proof_capture(
        "E2",
        format!(
            "wt open exit: {}\nerror: {}\nsession exists after open: false",
            output.status.code().unwrap(),
            message
        ),
    );
}

#[test]
fn nonexistent_agent_fails_startup_without_recording_and_later_open_creates_a_shell() {
    let Some(private) = PrivateTmux::new(false, false) else {
        eprintln!("skipping private-tmux missing-agent test: tmux is not installed");
        return;
    };
    let missing_agent = "wt-a62-agent-command-does-not-exist";
    write(
        &private.harness.home.join("config.toml"),
        &format!(
            "[session]\nbackend='tmux'\nattach=false\n[agents.missing]\nstart=['{missing_agent}']\nresume=['{missing_agent}']\n"
        ),
    );
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);

    let output = private
        .harness
        .wt()
        .args([
            "open",
            "repo",
            "--agent",
            "missing",
            "--no-attach",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "SESSION_CREATE_FAILED");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("startup observation window"), "{message}");
    assert!(message.contains(missing_agent), "{message}");
    let session = wt_core::session::name("repo", "canonical");
    assert!(!private.has_session(&session));

    let registry = wt_sys::fsx::read_json::<wt_core::model::Registry>(
        &private.harness.home.join("registry.json"),
        "REGISTRY_CORRUPT",
    )
    .unwrap()
    .unwrap();
    let tree = registry
        .trees
        .iter()
        .find(|tree| tree.label.as_str() == "repo" && tree.name == "canonical")
        .unwrap();
    assert_eq!(tree.agent, None);

    write(
        &private.harness.home.join("config.toml"),
        "[session]\nbackend='tmux'\nattach=false\n[shell]\nprogram='/bin/sh'\n",
    );
    let reopened = private.harness.json(&["open", "repo", "--no-attach"]);
    assert_eq!(reopened["data"]["sessions"][0]["created"], true);
    assert_eq!(
        reopened["data"]["sessions"][0]["agent"],
        serde_json::Value::Null
    );
    assert!(private.has_session(&session));
}

#[test]
fn agent_exit_leaves_an_assembled_shell_and_open_stays_idempotent() {
    let Some(private) = PrivateTmux::new(true, true) else {
        eprintln!("skipping private-tmux agent continuation test: tmux is not installed");
        return;
    };
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);

    let created = private
        .harness
        .json(&["new", "repo/work", "--no-sync", "--no-attach"]);
    let session = created["data"]["tree"]["session_name"].as_str().unwrap();
    private.wait_for_pane(session, "__AGENT_start__");
    assert!(private.has_session(session));
    assert!(private
        .tmux(&["resize-window", "-t", session, "-x", "240", "-y", "60"])
        .success());
    private.send_line(
        session,
        "printf '__AFTER_AGENT__ TARGET=%s CWD=%s\\n' \"$WT_TARGET\" \"$PWD\"",
    );
    let pane = private.wait_for_pane(session, "__AFTER_AGENT__");
    let tree = private.harness.home.join("trees/repo/work");
    assert!(
        pane.contains(&format!(
            "__AFTER_AGENT__ TARGET=repo/work CWD={}",
            tree.display()
        )),
        "post-agent shell did not retain the assembled environment:\n{pane}"
    );
    assert_eq!(private.agent_events(), ["start"]);

    let reopened = private.harness.json(&["open", "repo/work", "--no-attach"]);
    assert_eq!(reopened["data"]["sessions"][0]["created"], false);
    assert_eq!(reopened["data"]["sessions"][0]["existing"], true);
    assert_eq!(private.agent_events(), ["start"]);

    let request = private.harness.pty_request(&["open", "repo/work"]);
    let child = std::thread::spawn(move || {
        wt_sys::proc::pty_capture(&request, b"", Duration::from_secs(15)).unwrap()
    });
    private.wait_for_client(session);
    assert_eq!(private.agent_events(), ["start"]);
    private.send_line(session, "printf '__ATTACHED_AFTER_AGENT__\\n'");
    private.wait_for_pane(session, "__ATTACHED_AFTER_AGENT__");
    private.send_line(session, "exit");
    let output = child.join().unwrap();
    assert_eq!(output.child.code, Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("already open"));
}

#[test]
fn agent_that_runs_and_exits_nonzero_leaves_a_shell() {
    let Some(private) = PrivateTmux::new(false, false) else {
        eprintln!("skipping private-tmux nonzero-agent test: tmux is not installed");
        return;
    };
    write(
        &private.harness.home.join("config.toml"),
        "[session]\nbackend='tmux'\nattach=false\n[shell]\nprogram='/bin/sh'\n[agents.nonzero]\nstart=['/bin/sh','-c',\"printf '__AGENT_EXIT_3__\\\\n'; exit 3\"]\nresume=['/bin/sh','-c','exit 3']\n",
    );
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);

    let opened = private
        .harness
        .json(&["open", "repo", "--agent", "nonzero", "--no-attach"]);
    let session = opened["data"]["sessions"][0]["name"].as_str().unwrap();
    private.wait_for_pane(session, "__AGENT_EXIT_3__");
    assert!(private.has_session(session));
    private.send_line(session, "printf '__SHELL_AFTER_EXIT_3__\\n'");
    private.wait_for_pane(session, "__SHELL_AFTER_EXIT_3__");
    assert!(private.has_session(session));
}

#[test]
fn shell_only_session_keeps_its_direct_launch_argv() {
    let harness = Harness::new();
    write(
        &harness.home.join("config.toml"),
        "[session]\nbackend='tmux'\nattach=false\n[shell]\nprogram='/bin/sh'\n",
    );
    let repo = harness.repo("repo", "");
    harness.register(&repo);

    let opened = harness.json(&["open", "repo", "--no-attach"]);
    let session = opened["data"]["sessions"][0]["name"].as_str().unwrap();
    let argv =
        wt_sys::fsx::read_string(&harness.shim_state.join("tmux").join(session).join("argv"))
            .unwrap()
            .unwrap();
    let argv = argv.lines().collect::<Vec<_>>();
    let command_end = argv
        .iter()
        .position(|arg| *arg == ";")
        .expect("tmux command separator");
    assert_eq!(
        &argv[command_end - 6..command_end],
        &["exec", "--no-gate", "repo", "--", "/bin/sh", "-i"]
    );
}

impl Drop for PrivateTmux {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

#[test]
fn new_attaches_and_the_live_pane_has_the_tree_environment() {
    let Some(private) = PrivateTmux::new(false, true) else {
        eprintln!("skipping private-tmux session test: tmux is not installed");
        return;
    };
    let repo = private.harness.repo("repo", SESSION_CONFIG);
    let bin = repo.join("bin");
    wt_sys::fsx::create_private_dir(&bin).unwrap();
    write_executable(&bin.join("tree-tool"), "#!/bin/sh\nexit 0\n");
    common::git(&repo, &["add", "bin/tree-tool"]);
    common::git(&repo, &["commit", "-qm", "add tree tool"]);
    common::git(&repo, &["push", "-q", "origin", "main"]);
    private.harness.register(&repo);

    let session = wt_core::session::name("repo", "work");
    let request = private
        .harness
        .pty_request(&["new", "repo/work", "--no-sync"]);
    let child = std::thread::spawn(move || {
        wt_sys::proc::pty_capture(&request, b"", Duration::from_secs(15)).unwrap()
    });
    private.wait_for_session(&session);
    assert!(!child.is_finished(), "new exited before attaching to tmux");
    private.wait_for_client(&session);
    let attached_clients = private.clients(&session);
    assert_eq!(attached_clients, 1, "new did not attach its client");
    assert!(private
        .tmux(&["resize-window", "-t", &session, "-x", "240", "-y", "60"])
        .success());

    private.send_line(
        &session,
        "command -v tree-tool; printf 'ROOT=%s PORT=%s RAW=%s\\n' \"$WT_ROOT\" \"$APP_PORT\" \"${WT_PORT_HTTP-unset}\"; printf '__TREE_ENV__\\n'",
    );
    let pane = private.wait_for_pane(&session, "__TREE_ENV__");
    let tree = private.harness.home.join("trees/repo/work");
    assert!(
        pane.contains(&tree.join("bin/tree-tool").to_string_lossy().to_string()),
        "pane did not resolve the tree binary:\n{pane}"
    );
    assert!(
        pane.contains(&format!("ROOT={}", tree.display())),
        "pane did not contain WT_ROOT:\n{pane}"
    );
    assert!(
        pane.contains("PORT=20016"),
        "pane did not contain the port:\n{pane}"
    );
    assert!(
        pane.contains("RAW=unset"),
        "pane exported WT_PORT_HTTP:\n{pane}"
    );
    // The pane also carries the echoed command, wrapped at whatever width the
    // terminal had when it was typed, and the host shell's prompt — `sh-3.2$`
    // on macOS, `$` on Linux. Neither is evidence of anything this proves, so
    // the snapshot holds the command's output alone.
    let lines = pane.lines().collect::<Vec<_>>();
    let first = lines
        .iter()
        .position(|line| line.contains("bin/tree-tool"))
        .expect("pane did not echo the resolved tree binary");
    let last = lines
        .iter()
        .position(|line| line.trim() == "__TREE_ENV__")
        .expect("pane did not reach the sentinel");
    insta::assert_snapshot!(
        "session_tree_environment",
        lines[first..=last]
            .join("\n")
            .replace(
                &private.harness.root.to_string_lossy().to_string(),
                "<ROOT>"
            )
            .trim_end()
    );

    private.send_line(&session, "exit");
    let output = child.join().unwrap();
    assert_eq!(output.child.code, Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Created repo/work"));
    common::proof_capture(
        "B1",
        format!(
            "attached clients: {attached_clients}\n{}",
            String::from_utf8_lossy(&output.stdout)
                .split('\u{1b}')
                .next()
                .unwrap_or_default()
                .replace(
                    &private.harness.root.to_string_lossy().to_string(),
                    "<ROOT>"
                )
                .trim_end()
        ),
    );
    common::proof_capture(
        "B2",
        pane.replace(
            &private.harness.root.to_string_lossy().to_string(),
            "<ROOT>",
        )
        .trim_end(),
    );

    let canonical = wt_core::session::name("repo", "canonical");
    let request = private.harness.pty_request(&["open", "repo"]);
    let child = std::thread::spawn(move || {
        wt_sys::proc::pty_capture(&request, b"", Duration::from_secs(15)).unwrap()
    });
    private.wait_for_session(&canonical);
    private.wait_for_client(&canonical);
    private.send_line(&canonical, "printf '__OPENED_%s__\\n' SHELL");
    let opened_pane = private.wait_for_pane(&canonical, "__OPENED_SHELL__");
    private.send_line(&canonical, "exit");
    let output = child.join().unwrap();
    assert_eq!(output.child.code, Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("no agent was selected"));
    common::proof_capture(
        "A2",
        opened_pane
            .replace(
                &private.harness.root.to_string_lossy().to_string(),
                "<ROOT>",
            )
            .trim_end(),
    );
}

#[test]
fn agents_start_only_for_new_sessions_and_open_all_resumes_recorded_agents() {
    let Some(private) = PrivateTmux::new(true, true) else {
        eprintln!("skipping private-tmux agent test: tmux is not installed");
        return;
    };
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);

    let one = private
        .harness
        .json(&["new", "repo/one", "--no-sync", "--no-attach"]);
    let one_session = one["data"]["tree"]["session_name"].as_str().unwrap();
    private.wait_for_pane(one_session, "__AGENT_start__");
    assert_eq!(private.agent_events(), ["start"]);

    let attached = private.harness.json(&["open", "repo/one", "--no-attach"]);
    assert_eq!(attached["data"]["sessions"][0]["existing"], true);
    assert_eq!(private.agent_events(), ["start"]);

    private
        .harness
        .json(&["new", "repo/two", "--no-sync", "--no-open"]);
    assert_eq!(private.agent_events(), ["start"]);
    private.harness.json(&["close", "repo/one"]);

    let all = private.harness.json(&["open", "--all"]);
    assert_eq!(all["data"]["sessions"].as_array().unwrap().len(), 3);
    for session in all["data"]["sessions"].as_array().unwrap() {
        let marker = if session["target"] == "repo/one" {
            "__AGENT_resume__"
        } else {
            "__AGENT_start__"
        };
        private.wait_for_pane(session["name"].as_str().unwrap(), marker);
    }
    let events = private.agent_events();
    assert_eq!(events.iter().filter(|event| *event == "resume").count(), 1);
    assert_eq!(events.iter().filter(|event| *event == "start").count(), 3);
    common::proof_capture("B3", format!("agent events: {}", events.join(", ")));

    let non_json = private
        .harness
        .wt()
        .args(["open", "--all"])
        .output()
        .unwrap();
    assert!(non_json.status.success());
    let client_count = private.clients(one_session);
    assert_eq!(client_count, 0, "open --all attached a tmux client");
    common::proof_capture(
        "B6",
        format!(
            "{}\nattached clients: {client_count}",
            String::from_utf8_lossy(&non_json.stdout)
        ),
    );

    let Some(shells) = PrivateTmux::new(false, true) else {
        unreachable!("tmux availability cannot change within the test")
    };
    let shell_repo = shells.harness.repo("shells", "");
    shells.harness.register(&shell_repo);
    let opened = shells.harness.json(&["open", "shells", "--no-attach"]);
    let shell_session = opened["data"]["sessions"][0]["name"].as_str().unwrap();
    shells.wait_for_session(shell_session);
    assert!(shells.agent_events().is_empty());
}

#[derive(Clone, Copy)]
struct MatrixRow {
    name: &'static str,
    terminal: bool,
    activation: bool,
    json: bool,
    backend_none: bool,
    no_open: bool,
    no_attach: bool,
    setting_attach: bool,
    created: bool,
    attached: bool,
    agent_started: bool,
}

#[test]
fn session_creation_and_attachment_matrix() {
    let rows = [
        MatrixRow {
            name: "terminal",
            terminal: true,
            activation: false,
            json: false,
            backend_none: false,
            no_open: false,
            no_attach: false,
            setting_attach: true,
            created: true,
            attached: true,
            agent_started: true,
        },
        MatrixRow {
            name: "no-terminal",
            terminal: false,
            activation: false,
            json: false,
            backend_none: false,
            no_open: false,
            no_attach: false,
            setting_attach: true,
            created: true,
            attached: false,
            agent_started: true,
        },
        MatrixRow {
            name: "activation",
            terminal: true,
            activation: true,
            json: false,
            backend_none: false,
            no_open: false,
            no_attach: false,
            setting_attach: true,
            created: true,
            attached: false,
            agent_started: true,
        },
        MatrixRow {
            name: "json",
            terminal: true,
            activation: false,
            json: true,
            backend_none: false,
            no_open: false,
            no_attach: false,
            setting_attach: true,
            created: true,
            attached: false,
            agent_started: true,
        },
        MatrixRow {
            name: "backend-none",
            terminal: false,
            activation: false,
            json: false,
            backend_none: true,
            no_open: false,
            no_attach: false,
            setting_attach: true,
            created: false,
            attached: false,
            agent_started: false,
        },
        MatrixRow {
            name: "no-open",
            terminal: false,
            activation: false,
            json: false,
            backend_none: false,
            no_open: true,
            no_attach: false,
            setting_attach: true,
            created: false,
            attached: false,
            agent_started: false,
        },
        MatrixRow {
            name: "no-attach",
            terminal: false,
            activation: false,
            json: false,
            backend_none: false,
            no_open: false,
            no_attach: true,
            setting_attach: true,
            created: true,
            attached: false,
            agent_started: true,
        },
        MatrixRow {
            name: "attach-setting-false",
            terminal: true,
            activation: false,
            json: false,
            backend_none: false,
            no_open: false,
            no_attach: false,
            setting_attach: false,
            created: true,
            attached: false,
            agent_started: true,
        },
    ];

    let mut observed = Vec::new();
    for row in rows {
        let Some(private) = PrivateTmux::new(true, row.setting_attach) else {
            eprintln!("skipping private-tmux matrix: tmux is not installed");
            return;
        };
        if row.backend_none {
            let config = wt_sys::fsx::read_string(&private.harness.home.join("config.toml"))
                .unwrap()
                .unwrap()
                .replace("backend='tmux'", "backend='none'");
            write(&private.harness.home.join("config.toml"), &config);
        }
        let repo = private.harness.repo("repo", "");
        private.harness.register(&repo);
        let session = wt_core::session::name("repo", row.name);
        let mut args = vec!["new", row.name, "--no-sync"];
        let target = format!("repo/{}", row.name);
        args[1] = &target;
        if row.no_open {
            args.push("--no-open");
        }
        if row.no_attach {
            args.push("--no-attach");
        }
        if row.json {
            args.push("--json");
        }

        let mut attached_child = None;
        if row.terminal && row.attached {
            let request = private.harness.pty_request(&args);
            attached_child = Some(std::thread::spawn(move || {
                wt_sys::proc::pty_capture(&request, b"", Duration::from_secs(15)).unwrap()
            }));
            private.wait_for_pane(&session, "__AGENT_start__");
        } else if row.terminal {
            let mut request = private.harness.pty_request(&args);
            if row.activation {
                request
                    .env
                    .insert("WT_ACTIVATION".to_owned(), "active".to_owned());
            }
            let output = wt_sys::proc::pty_capture(&request, b"", Duration::from_secs(15)).unwrap();
            assert_eq!(output.child.code, Some(0), "matrix row {}", row.name);
        } else {
            let output = private.harness.wt().args(&args).output().unwrap();
            assert!(output.status.success(), "matrix row {}", row.name);
        }

        let created = private.has_session(&session);
        assert_eq!(created, row.created, "matrix row {}", row.name);
        if row.agent_started {
            private.wait_for_pane(&session, "__AGENT_start__");
        }
        if row.attached {
            private.wait_for_client(&session);
        }
        let attached = private.clients(&session) > 0;
        assert_eq!(attached, row.attached, "matrix row {}", row.name);
        let agent_started = !private.agent_events().is_empty();
        assert_eq!(agent_started, row.agent_started, "matrix row {}", row.name);
        if let Some(child) = attached_child {
            private.send_line(&session, "exit");
            assert_eq!(child.join().unwrap().child.code, Some(0));
        }
        observed.push(format!(
            "{:<20} created={} attached={} agent_started={}",
            row.name, created, attached, agent_started
        ));
    }
    common::proof_capture("B4", observed.join("\n"));
}

#[test]
fn remove_closes_the_private_tmux_session() {
    let Some(private) = PrivateTmux::new(false, true) else {
        eprintln!("skipping private-tmux remove test: tmux is not installed");
        return;
    };
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);
    let created = private
        .harness
        .json(&["new", "repo/work", "--no-sync", "--no-attach"]);
    let session = created["data"]["tree"]["session_name"].as_str().unwrap();
    assert!(private.has_session(session));
    private
        .harness
        .json(&["remove", "repo/work", "--yes", "--force"]);
    assert!(!private.has_session(session));
    common::proof_capture(
        "B9",
        format!("session {session}\nbefore remove: present\nafter remove: absent"),
    );
}

#[test]
fn backend_none_never_invokes_tmux_for_truth_or_teardown() {
    let harness = Harness::new();
    write(
        &harness.home.join("config.toml"),
        "[session]\nbackend='none'\n",
    );
    let repo = harness.repo("repo", "");
    harness.register(&repo);
    harness.json(&["new", "repo/work", "--no-sync"]);
    let record = harness.shim_state.join("tmux-invocations");
    write_executable(
        &harness.shims.join("tmux"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nexit 99\n",
            record.display()
        ),
    );

    harness.json(&["list"]);
    harness.json(&["remove", "repo/work", "--yes", "--force"]);
    harness.json(&["prune", "--yes"]);
    assert_eq!(wt_sys::fsx::read_string(&record).unwrap(), None);
    for verb in ["open", "close"] {
        harness
            .wt()
            .args([verb, "repo"])
            .assert()
            .code(5)
            .stderr(predicates::str::contains("session.backend"))
            .stderr(predicates::str::contains("backend = \"tmux\""));
    }
    assert_eq!(wt_sys::fsx::read_string(&record).unwrap(), None);
    common::proof_capture("B7", "list/remove/prune/open/close tmux invocations: 0");
}

#[test]
fn new_starts_build_in_setup_window_and_shims_report_progress_and_failure() {
    let Some(private) = PrivateTmux::new(false, false) else {
        eprintln!("skipping private-tmux build test: tmux is not installed");
        return;
    };
    let worker = private.harness.shims.join("build-worker");
    let events = private.harness.root.join("build-events");
    let release = private.harness.root.join("release-build");
    write_executable(
        &worker,
        r#"#!/bin/sh
if [ "$1" = prepare ]; then
  printf 'prepare\n' >> "$2"
  exit 0
fi
printf 'build-start\n' >> "$2"
i=0
while [ ! -f "$3" ] && [ "$i" -lt 500 ]; do
  i=$((i + 1))
  sleep 0.02
done
[ -f "$3" ] || exit 98
printf 'build-failed\n' >> "$2"
exit 9
"#,
    );
    let config = format!(
        r#"
bin = ["bin"]
commands = ["orbit"]
[task.prepare]
run = ["{}", "prepare", "{}", "{}"]
[task.build]
needs = ["prepare"]
run = ["{}", "build", "{}", "{}"]
"#,
        worker.display(),
        events.display(),
        release.display(),
        worker.display(),
        events.display(),
        release.display(),
    );
    let repo = private.harness.repo("repo", &config);
    write_executable(
        &private.harness.shims.join("orbit"),
        "#!/bin/sh\nprintf 'INSTALLED\\n'\n",
    );
    private.harness.register(&repo);

    let output = private
        .harness
        .wt()
        .args(["new", "repo/work", "--no-sync", "--no-attach"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let observed = wt_sys::fsx::read_string(&events)
            .unwrap()
            .unwrap_or_default();
        if observed.contains("build-start") {
            assert_eq!(observed, "prepare\nbuild-start\n");
            break;
        }
        assert!(Instant::now() < deadline, "build never started: {observed}");
        std::thread::sleep(Duration::from_millis(20));
    }

    let progress = private
        .harness
        .wt()
        .args(["exec", "repo/work", "--", "orbit"])
        .output()
        .unwrap();
    assert_eq!(progress.status.code(), Some(5));
    let progress_text = String::from_utf8_lossy(&progress.stderr);
    assert!(
        progress_text.contains("build is in progress"),
        "{progress_text}"
    );
    assert!(progress_text.contains("wt:setup"), "{progress_text}");
    assert!(progress_text.contains("wt-setup.log"), "{progress_text}");
    assert!(
        progress_text.contains("wt build repo/work"),
        "{progress_text}"
    );
    assert!(
        progress_text.contains(
            &private
                .harness
                .shims
                .join("orbit")
                .to_string_lossy()
                .to_string()
        ),
        "{progress_text}"
    );

    write(&release, "go\n");
    let status = private
        .harness
        .home
        .join("trees/repo/work/.wt/build.status");
    let deadline = Instant::now() + Duration::from_secs(10);
    while wt_sys::fsx::read_string(&status).unwrap().as_deref() != Some("failed\n") {
        assert!(
            Instant::now() < deadline,
            "build status never became failed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let failed = private
        .harness
        .wt()
        .args(["exec", "repo/work", "--", "orbit"])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(5));
    let failed_text = String::from_utf8_lossy(&failed.stderr);
    assert!(failed_text.contains("wt build repo/work"), "{failed_text}");
    assert!(
        failed_text.contains(
            &private
                .harness
                .shims
                .join("orbit")
                .to_string_lossy()
                .to_string()
        ),
        "{failed_text}"
    );
    assert!(!failed_text.contains("wt:setup"), "{failed_text}");
    assert!(!failed_text.contains("wt-setup.log"), "{failed_text}");

    let target = wt_core::model::Target::parse("repo/work").unwrap();
    let state = wt_sys::fsx::read_json::<wt_core::lifecycle::TreeState>(
        &private
            .harness
            .home
            .join(wt_core::model::tree_state_path(&target)),
        "STATE_CORRUPT",
    )
    .unwrap()
    .unwrap();
    let build = state.build.unwrap();
    assert_eq!(build.window.as_deref(), Some("wt:setup"));
    assert!(Path::new(&build.log).exists());
    assert_eq!(state.phase, wt_core::lifecycle::StatePhase::Ready);
    common::proof_capture(
        "D2",
        format!(
            "events:\n{}state window: {:?}\nstate log: {}",
            std::fs::read_to_string(&events).unwrap(),
            build.window,
            build.log
        )
        .replace(
            &std::fs::canonicalize(&private.harness.root)
                .unwrap_or_else(|_| private.harness.root.clone())
                .to_string_lossy()
                .to_string(),
            "<ROOT>",
        )
        .replace(
            &private.harness.root.to_string_lossy().to_string(),
            "<ROOT>",
        ),
    );
    common::proof_capture(
        "D3",
        format!("running refusal:\n{progress_text}\nterminal refusal:\n{failed_text}")
            .replace(
                &std::fs::canonicalize(&private.harness.root)
                    .unwrap_or_else(|_| private.harness.root.clone())
                    .to_string_lossy()
                    .to_string(),
                "<ROOT>",
            )
            .replace(
                &private.harness.root.to_string_lossy().to_string(),
                "<ROOT>",
            ),
    );
}

#[test]
fn new_rejects_agent_while_open_agent_starts_the_requested_recipe() {
    let Some(private) = PrivateTmux::new(false, true) else {
        eprintln!("skipping private-tmux CLI test: tmux is not installed");
        return;
    };
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);
    let rejected = private
        .harness
        .wt()
        .args(["new", "repo/work", "--agent", "probe"])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("unexpected argument '--agent'"));
    assert!(!private.has_session(&wt_core::session::name("repo", "work")));

    let opened = private
        .harness
        .json(&["open", "repo", "--agent", "probe", "--no-attach"]);
    let session = opened["data"]["sessions"][0]["name"].as_str().unwrap();
    let pane = private.wait_for_pane(session, "__AGENT_start__");
    assert_eq!(private.agent_events(), ["start"]);
    common::proof_capture(
        "B5",
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&rejected.stderr),
            pane.replace(
                &private.harness.root.to_string_lossy().to_string(),
                "<ROOT>"
            )
            .trim_end()
        ),
    );
}

fn find_tmux() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join("tmux"))
        .find(|path| wt_sys::fsx::is_executable_file(path).unwrap_or(false))
        .or_else(|| {
            [
                "/opt/homebrew/bin/tmux",
                "/usr/local/bin/tmux",
                "/usr/bin/tmux",
            ]
            .into_iter()
            .map(Path::new)
            .find(|path| wt_sys::fsx::is_executable_file(path).unwrap_or(false))
            .map(Path::to_path_buf)
        })
}
