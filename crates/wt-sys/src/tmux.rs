use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use wt_core::{CoreError, ExitClass};

use crate::proc::{self, CommandRequest, ProcessOutput};
use crate::Result;

const SESSION_START_OBSERVATION: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub struct Tmux {
    program: OsString,
    deadline: Duration,
    env: BTreeMap<String, String>,
}

impl Tmux {
    pub fn new(program: impl Into<OsString>, deadline: Duration) -> Self {
        Self {
            program: program.into(),
            deadline,
            env: BTreeMap::new(),
        }
    }

    /// Adds explicit subprocess environment entries, primarily for shims.
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Checks tmux's file format version and rejects versions below 3.2.
    pub fn check_version(&self) -> Result<(u32, u32)> {
        let output = self.status(&["-V"])?;
        let output = success(output, "read tmux version")?;
        let value = String::from_utf8_lossy(&output.stdout);
        let version = value
            .split_whitespace()
            .find_map(|part| {
                part.char_indices()
                    .find(|(_, character)| character.is_ascii_digit())
                    .map(|(index, _)| &part[index..])
            })
            .unwrap_or_default();
        let mut parts = version.split('.');
        let major = numeric_prefix(parts.next().unwrap_or_default());
        let minor = numeric_prefix(parts.next().unwrap_or_default());
        if (major, minor) < (3, 2) {
            return Err(CoreError::new(
                ExitClass::State,
                "TMUX_OLD",
                format!("tmux {major}.{minor} is older than 3.2"),
                "upgrade tmux to version 3.2 or newer",
            ));
        }
        Ok((major, minor))
    }

    /// Lists live session names in one query, so a fleet view costs one
    /// tmux subprocess instead of one per tree. No running server means
    /// no sessions, which tmux reports as its ordinary absent status.
    pub fn session_names(&self) -> Result<std::collections::BTreeSet<String>> {
        let output = self.status(&["list-sessions", "-F", "#{session_name}"])?;
        match output.child.code {
            Some(0) => Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_owned)
                .collect()),
            Some(1) => Ok(std::collections::BTreeSet::new()),
            _ => Err(tmux_failed("list sessions", &output)),
        }
    }

    /// Runs `has-session`, mapping tmux's ordinary absent status to false.
    pub fn has_session(&self, session: &str) -> Result<bool> {
        let exact = exact_target(session);
        let output = self.status(&["has-session", "-t", &exact])?;
        match output.child.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(tmux_failed("query session", &output)),
        }
    }

    /// Creates a detached session; the inner wt door assembles its environment.
    pub fn new_session(
        &self,
        session: &str,
        cwd: &Path,
        home: &Path,
        capture: &Path,
        command: &[OsString],
    ) -> Result<()> {
        let Some((program, args)) = command.split_first() else {
            return Err(CoreError::new(
                ExitClass::State,
                "CONFIG_INVALID",
                "tmux session command is empty",
                "configure an agent start command",
            ));
        };
        let mut request = CommandRequest::new(&self.program);
        request.env = self.env.clone();
        request.args = proc::os_args(&["new-session", "-d", "-s", session, "-c"]);
        request.args.push(cwd.as_os_str().to_owned());
        request.args.push(OsString::from("-e"));
        request
            .args
            .push(OsString::from(format!("WT_HOME={}", home.display())));
        let gate = capture.with_extension("gate");
        request.args.push(OsString::from("--"));
        request.args.push(OsString::from("/bin/sh"));
        request.args.extend(proc::os_args(&[
            "-c",
            "gate=$1; shift; i=0; while [ ! -f \"$gate\" ] && [ \"$i\" -lt 1000 ]; do i=$((i + 1)); sleep 0.01; done; [ -f \"$gate\" ] || { echo 'wt session bootstrap gate timed out' >&2; exit 125; }; exec \"$@\"",
            "wt-session-bootstrap",
        ]));
        request.args.push(gate.as_os_str().to_owned());
        request.args.push(program.clone());
        request.args.extend(args.iter().cloned());
        request.args.push(OsString::from(";"));
        request.args.extend(proc::os_args(&[
            "pipe-pane",
            "-o",
            "-t",
            &exact_pane_target(session),
            &format!("cat > {}", shell_quote(capture)),
        ]));
        let output = proc::capture(&request, self.deadline).map_err(tool_error)?;
        success(output, "create tmux session")?;
        crate::fsx::write_store(&gate, b"ready\n")?;
        // A bounded observation window catches immediate bootstrap failures; it
        // does not prove that a session will remain alive after this method.
        let deadline = Instant::now() + SESSION_START_OBSERVATION;
        loop {
            if !self.has_session(session)? {
                let reason = captured_reason(capture, self.deadline);
                let _ = crate::fsx::remove_path(&gate);
                let _ = crate::fsx::remove_path(capture);
                return Err(CoreError::new(
                    ExitClass::External,
                    "SESSION_CREATE_FAILED",
                    format!(
                        "tmux session `{session}` exited during the startup observation window: {reason}"
                    ),
                    "fix the session command or WT_HOME and retry",
                ));
            }
            if Instant::now() >= deadline {
                let _ = crate::fsx::remove_path(&gate);
                let _ = crate::fsx::remove_path(capture);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kills one session; callers use `has_session` for idempotence.
    pub fn kill_session(&self, session: &str) -> Result<()> {
        let exact = exact_target(session);
        let output = self.status(&["kill-session", "-t", &exact])?;
        success(output, "kill tmux session").map(|_| ())
    }

    /// Switches the current tmux client to the named session.
    pub fn switch_client(&self, session: &str) -> Result<()> {
        let exact = exact_target(session);
        let output = self.status(&["switch-client", "-t", &exact])?;
        success(output, "switch tmux client").map(|_| ())
    }

    /// Replaces wt with an attaching tmux client, preserving terminal semantics.
    pub fn attach_session(&self, session: &str) -> Result<()> {
        let exact = exact_target(session);
        let error = Command::new(&self.program)
            .args(["attach-session", "-t", &exact])
            .exec();
        Err(tool_error(CoreError::new(
            ExitClass::External,
            "SPAWN_FAILED",
            format!("could not attach tmux session: {error}"),
            "install tmux and retry",
        )))
    }

    fn status(&self, args: &[&str]) -> Result<ProcessOutput> {
        let mut request = CommandRequest::new(&self.program);
        request.args = proc::os_args(args);
        request.env = self.env.clone();
        proc::capture(&request, self.deadline).map_err(tool_error)
    }
}

fn exact_target(session: &str) -> String {
    format!("={session}")
}

fn exact_pane_target(session: &str) -> String {
    format!("={session}:")
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn captured_reason(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(1));
    loop {
        if let Ok(Some(text)) = crate::fsx::read_string(path) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_owned();
            }
        }
        if Instant::now() >= deadline {
            return "the pane exited without output".to_owned();
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn success(output: ProcessOutput, action: &str) -> Result<ProcessOutput> {
    if output.timed_out {
        return Err(CoreError::new(
            ExitClass::Timeout,
            "TIMEOUT",
            format!("{action} timed out"),
            "retry after tmux becomes responsive",
        ));
    }
    if !output.success() {
        return Err(tmux_failed(action, &output));
    }
    Ok(output)
}

fn tmux_failed(action: &str, output: &ProcessOutput) -> CoreError {
    CoreError::new(
        ExitClass::External,
        "TMUX_FAILED",
        format!(
            "{action} failed with status {}: {}",
            output.mapped_exit(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        "inspect tmux's error and retry",
    )
}

fn tool_error(error: CoreError) -> CoreError {
    if error.code.0 == "SPAWN_FAILED" {
        // Carry the operating system's reason forward: "not installed" and
        // "installed but momentarily unexecutable" are different problems and
        // a bare TOOL_MISSING cannot be told apart from either.
        CoreError::new(
            ExitClass::State,
            "TOOL_MISSING",
            format!("tmux is not installed or executable ({})", error.message),
            "install tmux 3.2 or newer",
        )
    } else {
        error
    }
}

fn numeric_prefix(value: &str) -> u32 {
    value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// The tmux options `setup` needs to know about before it suggests anything
/// (A76, §14.7).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveOptions {
    pub default_terminal: Option<String>,
    pub extended_keys: Option<String>,
    pub terminal_features: Option<String>,
    pub mouse: Option<String>,
}

/// Kills a probe server however the probe ends.
///
/// A server left behind by an early return would outlive the command and hold
/// the user's plugins in memory, so the kill is a destructor rather than a
/// step that a `?` can skip.
struct ProbeServer {
    program: OsString,
    socket: String,
    deadline: Duration,
}

impl Drop for ProbeServer {
    fn drop(&mut self) {
        let mut request = CommandRequest::new(self.program.clone());
        request.args = proc::os_args(&["-L", &self.socket, "kill-server"]);
        let _ = proc::capture(&request, self.deadline);
        // tmux leaves the socket file behind, and this probe runs on every
        // `doctor` with a tmux backend; one stale file per run would pile up.
        for directory in socket_directories() {
            let path = directory.join(&self.socket);
            // Only ever a socket. `remove_path` recurses into a directory,
            // and this path is predicted rather than observed: if anything
            // else owns it, deleting its contents is data loss for a cleanup
            // that was only ever cosmetic.
            if matches!(
                crate::fsx::path_kind(&path),
                Ok(crate::fsx::PathKind::Other)
            ) {
                let _ = crate::fsx::remove_path(&path);
            }
        }
    }
}

/// Where tmux keeps its sockets, following the same rules tmux itself uses.
fn socket_directories() -> Vec<std::path::PathBuf> {
    let uid = unsafe { libc::getuid() };
    let mut directories = Vec::new();
    if let Some(base) = std::env::var_os("TMUX_TMPDIR") {
        directories.push(std::path::PathBuf::from(base).join(format!("tmux-{uid}")));
    }
    directories.push(std::path::PathBuf::from(format!("/tmp/tmux-{uid}")));
    directories
}

/// Reads what a tmux configuration actually resolves to.
///
/// The values are read from a throwaway server on its own socket rather than
/// from the file, because a configuration's effect is the product of its
/// sourced fragments, its plugins and its ordering — none of which a textual
/// reading of one file can see. The user's own `run-shell` lines execute, as
/// they do at every tmux start, under this deadline.
pub fn probe_effective(
    program: impl Into<OsString>,
    config: Option<&Path>,
    deadline: Duration,
) -> Result<EffectiveOptions> {
    let program = program.into();
    let socket = format!("wt-probe-{}", std::process::id());
    let mut start = CommandRequest::new(program.clone());
    let mut args = vec!["-L".to_owned(), socket.clone()];
    if let Some(config) = config {
        args.push("-f".to_owned());
        args.push(config.to_string_lossy().into_owned());
    }
    // A detached session, not `start-server`: a server with no session exits
    // as soon as its invocation ends, so every later query would find nothing
    // running and report the configuration as empty.
    args.push("new-session".to_owned());
    args.push("-d".to_owned());
    start.args = args.iter().map(OsString::from).collect();

    let server = ProbeServer {
        program: program.clone(),
        socket: socket.clone(),
        deadline,
    };
    let started = proc::capture(&start, deadline)?;
    if !started.success() {
        drop(server);
        return Err(tmux_failed("start a configuration probe", &started));
    }

    // Queried one option at a time: scope differs between them, and a single
    // dump does not reliably show an option left at its default.
    let options = EffectiveOptions {
        default_terminal: show(&program, &socket, "-sv", "default-terminal", deadline),
        extended_keys: show(&program, &socket, "-sv", "extended-keys", deadline),
        terminal_features: show(&program, &socket, "-sv", "terminal-features", deadline),
        mouse: show(&program, &socket, "-gv", "mouse", deadline),
    };
    drop(server);
    Ok(options)
}

fn show(
    program: &OsString,
    socket: &str,
    scope: &str,
    option: &str,
    deadline: Duration,
) -> Option<String> {
    let mut request = CommandRequest::new(program.clone());
    request.args = proc::os_args(&["-L", socket, "show", scope, option]);
    let output = proc::capture(&request, deadline).ok()?;
    if !output.success() {
        return None;
    }
    // An array option such as `terminal-features` prints one entry per line;
    // rejoining them with the separator tmux accepts keeps the value in the
    // one form every reader parses.
    let value = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn stub(script: &str) -> (tempfile::TempDir, Tmux, PathBuf) {
        let dir = tempdir().unwrap();
        let program = dir.path().join("tmux");
        let record = dir.path().join("record");
        crate::stub::write(&program, script);
        // The production default is ten seconds (SPEC §13.3); using that
        // deadline also keeps these process tests stable under parallel load.
        (dir, Tmux::new(program, Duration::from_secs(10)), record)
    }

    use std::path::PathBuf;

    #[test]
    fn version_gate_accepts_32_and_rejects_old() {
        let (_dir, tmux, _) = stub("#!/bin/sh\nprintf 'tmux 3.2a\\n'\n");
        assert_eq!(tmux.check_version().unwrap(), (3, 2));
        let (_dir, tmux, _) = stub("#!/bin/sh\nprintf 'tmux 3.1\\n'\n");
        assert_eq!(tmux.check_version().unwrap_err().code.0, "TMUX_OLD");
    }

    #[test]
    fn new_session_records_cwd_env_and_child_argv() {
        let script = "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$(dirname \"$0\")/record\"\nexit 0\n";
        let (dir, tmux, record) = stub(script);
        tmux.new_session(
            "wt_test",
            Path::new("/tmp/tree"),
            Path::new("/tmp/wt-home"),
            &dir.path().join("capture"),
            &["wt".into(), "exec".into()],
        )
        .unwrap();
        let args = fs::read_to_string(record).unwrap();
        assert!(args.contains("-c\n/tmp/tree\n-e\nWT_HOME=/tmp/wt-home\n"));
        assert!(args.contains("/bin/sh\n-c\n"));
        assert!(args.contains("\nwt\nexec\n;\npipe-pane\n"));
        assert!(args.contains("pipe-pane\n-o\n-t\n=wt_test:\n"));
    }

    #[test]
    fn session_targets_are_exact_matches() {
        let script = "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/record\"\nexit 1\n";
        let (_dir, tmux, record) = stub(script);
        assert!(!tmux.has_session("repo/work").unwrap());
        assert_eq!(
            fs::read_to_string(record).unwrap(),
            "has-session\n-t\n=repo/work\n"
        );
    }

    #[test]
    fn version_gate_finds_a_numeric_version_after_a_vendor_prefix() {
        let (_dir, tmux, _) = stub("#!/bin/sh\nprintf 'tmux next-3.4\\n'\n");
        assert_eq!(tmux.check_version().unwrap(), (3, 4));
    }
}

#[cfg(test)]
mod probe_tests {
    #[test]
    fn socket_cleanup_never_touches_a_directory() {
        // The socket path is predicted from a pid, not observed. A directory
        // there belongs to something else, and `remove_path` would delete it
        // and everything under it.
        let temp = tempfile::tempdir().unwrap();
        let squatter = temp.path().join("wt-probe-1");
        std::fs::create_dir_all(squatter.join("nested")).unwrap();
        std::fs::write(squatter.join("nested/important"), b"keep me").unwrap();

        let kind = crate::fsx::path_kind(&squatter).unwrap();
        assert_eq!(kind, crate::fsx::PathKind::Directory);
        assert!(
            !matches!(kind, crate::fsx::PathKind::Other),
            "the cleanup predicate must reject a directory"
        );
        assert!(squatter.join("nested/important").exists());
    }
}
