mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{write, write_executable, Harness};
use wt_sys::proc::{self, CommandRequest, ProcessOutput};

const SESSION_CONFIG: &str = "bin=['bin']\nports=['http']\n";

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
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"{}\"\nprintf '__AGENT_%s__\\n' \"$1\"\nexec /bin/sh -i\n",
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
        let mut request = CommandRequest::new(&self.binary);
        request.args = vec![OsString::from("-L"), OsString::from(&self.socket)];
        request.args.extend(proc::os_args(args));
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
    assert_eq!(
        private.clients(&session),
        1,
        "new did not attach its client"
    );
    assert!(private
        .tmux(&["resize-window", "-t", &session, "-x", "240", "-y", "60"])
        .success());

    private.send_line(
        &session,
        "command -v tree-tool; printf 'ROOT=%s PORT=%s\\n' \"$WT_ROOT\" \"$WT_PORT_HTTP\"; printf '__TREE_ENV__\\n'",
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
    insta::assert_snapshot!(
        "session_tree_environment",
        pane.replace(
            &private.harness.root.to_string_lossy().to_string(),
            "<ROOT>"
        )
        .trim_end()
    );

    private.send_line(&session, "exit");
    let output = child.join().unwrap();
    assert_eq!(output.child.code, Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Created repo/work"));

    let canonical = wt_core::session::name("repo", "canonical");
    let request = private.harness.pty_request(&["open", "repo"]);
    let child = std::thread::spawn(move || {
        wt_sys::proc::pty_capture(&request, b"", Duration::from_secs(15)).unwrap()
    });
    private.wait_for_session(&canonical);
    private.wait_for_client(&canonical);
    private.send_line(&canonical, "printf '__OPENED_SHELL__\\n'");
    private.wait_for_pane(&canonical, "__OPENED_SHELL__");
    private.send_line(&canonical, "exit");
    let output = child.join().unwrap();
    assert_eq!(output.child.code, Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("no agent was selected"));
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

        assert_eq!(
            private.has_session(&session),
            row.created,
            "matrix row {}",
            row.name
        );
        if row.agent_started {
            private.wait_for_pane(&session, "__AGENT_start__");
        }
        if row.attached {
            private.wait_for_client(&session);
        }
        assert_eq!(
            private.clients(&session) > 0,
            row.attached,
            "matrix row {}",
            row.name
        );
        assert_eq!(
            !private.agent_events().is_empty(),
            row.agent_started,
            "matrix row {}",
            row.name
        );
        if let Some(child) = attached_child {
            private.send_line(&session, "exit");
            assert_eq!(child.join().unwrap().child.code, Some(0));
        }
    }
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
}

#[test]
fn new_rejects_agent_while_open_agent_starts_the_requested_recipe() {
    let Some(private) = PrivateTmux::new(false, true) else {
        eprintln!("skipping private-tmux CLI test: tmux is not installed");
        return;
    };
    let repo = private.harness.repo("repo", "");
    private.harness.register(&repo);
    private
        .harness
        .wt()
        .args(["new", "repo/work", "--agent", "probe"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("unexpected argument '--agent'"));
    assert!(!private.has_session(&wt_core::session::name("repo", "work")));

    let opened = private
        .harness
        .json(&["open", "repo", "--agent", "probe", "--no-attach"]);
    let session = opened["data"]["sessions"][0]["name"].as_str().unwrap();
    private.wait_for_pane(session, "__AGENT_start__");
    assert_eq!(private.agent_events(), ["start"]);
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
