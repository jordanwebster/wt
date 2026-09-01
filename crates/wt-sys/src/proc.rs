use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use wt_core::resource::{ChildStatus, ExpandedCommand, Probe};
use wt_core::{CoreError, ExitClass};

use crate::Result;

const TERM_GRACE: Duration = Duration::from_secs(5);

/// Handles an invocation through `<root>/.wt/shims/<name>` before the CLI can
/// read configuration or acquire a lock. Returns `None` only for the ordinary
/// `wt` executable name.
pub fn owned_command_fast_path() -> Option<i32> {
    let mut argv = std::env::args_os();
    let argv0 = argv.next()?;
    let raw_invoked = PathBuf::from(&argv0);
    let name = raw_invoked.file_name()?.to_os_string();
    if raw_invoked
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty())
        && name == "wt"
    {
        return None;
    }
    let invoked = if raw_invoked
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty())
    {
        let Some(invoked) = matching_path_shim(&name) else {
            return Some(shim_refusal(format!(
                "SHIM_INVOCATION_INVALID: bare argv[0] `{}` has no matching <root>/.wt/shims symlink on PATH; restore the door PATH prefix before invoking owned commands",
                raw_invoked.display()
            )));
        };
        invoked
    } else {
        raw_invoked
    };
    let parent = invoked.parent();
    let has_expected_parent = parent.is_some_and(|parent| {
        parent.file_name().is_some_and(|part| part == "shims")
            && parent
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|part| part == ".wt")
    });
    if !has_expected_parent {
        if name == "wt" {
            return None;
        }
        return Some(shim_refusal(format!(
            "SHIM_INVOCATION_INVALID: `{}` is not an absolute <root>/.wt/shims/<name> path",
            invoked.display()
        )));
    }
    if !invoked.is_absolute() {
        return Some(shim_refusal(format!(
            "SHIM_INVOCATION_INVALID: relative shim path `{}` is not trusted",
            invoked.display()
        )));
    }
    let parent = parent.expect("expected shim path has a parent");
    if !parent.is_dir() {
        return Some(shim_refusal(format!(
            "SHIM_INVOCATION_INVALID: shim parent `{}` is absent",
            parent.display()
        )));
    }
    let link_target = match std::fs::symlink_metadata(&invoked) {
        Ok(metadata) if metadata.file_type().is_symlink() => match std::fs::read_link(&invoked) {
            Ok(target) => target,
            Err(error) => {
                return Some(shim_refusal(format!(
                    "SHIM_INVOCATION_INVALID: cannot read shim `{}`: {error}",
                    invoked.display()
                )))
            }
        },
        Ok(_) => {
            return Some(shim_refusal(format!(
                "SHIM_INVOCATION_INVALID: copied shim `{}` is not a symlink",
                invoked.display()
            )))
        }
        Err(error) => {
            return Some(shim_refusal(format!(
                "SHIM_INVOCATION_INVALID: cannot inspect shim `{}`: {error}",
                invoked.display()
            )))
        }
    };
    if !link_target.is_absolute()
        || std::fs::symlink_metadata(&link_target)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(true)
    {
        return Some(shim_refusal(format!(
            "SHIM_INVOCATION_INVALID: shim `{}` must point directly to an absolute wt binary",
            invoked.display()
        )));
    }
    let running = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return Some(shim_refusal(format!(
                "SHIM_INVOCATION_INVALID: cannot resolve the running binary: {error}"
            )))
        }
    };
    let target_matches = std::fs::canonicalize(&link_target)
        .ok()
        .zip(std::fs::canonicalize(&running).ok())
        .is_some_and(|(target, running)| target == running);
    if !target_matches {
        return Some(shim_refusal(format!(
            "SHIM_INVOCATION_INVALID: shim `{}` does not point to this wt binary",
            invoked.display()
        )));
    }
    let root = parent
        .parent()
        .and_then(Path::parent)
        .expect("expected shim path has a root");
    let wt_bin = std::env::var_os("WT_BIN").unwrap_or_default();
    let bins = std::env::split_paths(&wt_bin).collect::<Vec<_>>();
    if bins.iter().any(|bin| !contained_absolute(root, bin)) {
        return Some(shim_refusal(format!(
            "SHIM_INVOCATION_INVALID: WT_BIN does not describe directories inside `{}`",
            root.display()
        )));
    }
    for bin in &bins {
        let candidate = bin.join(&name);
        if executable(&candidate) {
            let mut command = Command::new(&candidate);
            command.arg0(&argv0).args(argv);
            let error = command.exec();
            return Some(shim_refusal(format!(
                "COMMAND_EXEC_FAILED: could not execute `{}`: {error}",
                candidate.display()
            )));
        }
    }
    let installed = find_installed_copy(&invoked, &name);
    let target = std::env::var("WT_TARGET").unwrap_or_else(|_| root.display().to_string());
    let searched = if bins.is_empty() {
        "<none>".to_owned()
    } else {
        bins.iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let explicit = installed.map_or_else(
        || "; no installed copy was found elsewhere on PATH".to_owned(),
        |path| {
            format!(
                "; run the installed copy explicitly as `{}`",
                path.display()
            )
        },
    );
    let progress = recorded_build_progress(root, &target).unwrap_or_default();
    Some(shim_refusal(format!(
        "COMMAND_NOT_BUILT: `{}` belongs to tree `{target}` but was not found; searched: {searched}; run `wt build {target}`{explicit}{progress}",
        name.to_string_lossy()
    )))
}

fn matching_path_shim(name: &std::ffi::OsStr) -> Option<PathBuf> {
    let running = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|directory| {
            directory.is_absolute()
                && directory.file_name().is_some_and(|part| part == "shims")
                && directory
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|part| part == ".wt")
        })
        .map(|directory| directory.join(name))
        .find(|candidate| {
            let Ok(metadata) = std::fs::symlink_metadata(candidate) else {
                return false;
            };
            if !metadata.file_type().is_symlink() {
                return false;
            }
            let Ok(target) = std::fs::read_link(candidate) else {
                return false;
            };
            target.is_absolute()
                && std::fs::symlink_metadata(&target)
                    .is_ok_and(|metadata| !metadata.file_type().is_symlink())
                && std::fs::canonicalize(target).is_ok_and(|target| target == running)
        })
}

fn recorded_build_progress(root: &Path, target: &str) -> Option<String> {
    if std::fs::read_to_string(root.join(".wt/build.status"))
        .ok()?
        .trim()
        != "running"
    {
        return None;
    }
    let home = PathBuf::from(std::env::var_os("WT_HOME")?);
    let target_value = wt_core::model::Target::parse(target).ok()?;
    let state_path = home.join(wt_core::model::tree_state_path(&target_value));
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(state_path).ok()?).ok()?;
    let build = state.get("build")?.as_object()?;
    let pid = u32::try_from(build.get("pid")?.as_u64()?).ok()?;
    if !process_alive(pid) {
        return None;
    }
    let log = build.get("log")?.as_str()?;
    Some(format!("; the build is in progress; watch log `{log}`"))
}

fn shim_refusal(message: String) -> i32 {
    eprintln!("wt: {message}");
    5
}

fn contained_absolute(root: &Path, candidate: &Path) -> bool {
    candidate.is_absolute()
        && candidate.starts_with(root)
        && !candidate.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

fn executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn find_installed_copy(invoked: &Path, name: &std::ffi::OsStr) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| {
            let directory = if directory.is_absolute() {
                directory
            } else {
                cwd.join(directory)
            };
            directory.join(name)
        })
        .find(|candidate| candidate != invoked && executable(candidate))
        .and_then(|path| std::fs::canonicalize(path).ok())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRequest {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub remove_env: Vec<OsString>,
    pub clear_env: bool,
}

impl CommandRequest {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            remove_env: Vec::new(),
            clear_env: false,
        }
    }

    pub fn expanded(
        command: &ExpandedCommand,
        cwd: impl Into<PathBuf>,
        env: BTreeMap<String, String>,
    ) -> Result<Self> {
        let (program, args) = match command {
            ExpandedCommand::Shell { shell } => (
                OsString::from("sh"),
                vec![OsString::from("-c"), OsString::from(shell)],
            ),
            ExpandedCommand::Argv { argv } => {
                let Some((program, args)) = argv.split_first() else {
                    return Err(CoreError::new(
                        ExitClass::State,
                        "CONFIG_INVALID",
                        "an argv command is empty",
                        "give the command at least one argv element",
                    ));
                };
                (
                    OsString::from(program),
                    args.iter().map(OsString::from).collect(),
                )
            }
        };
        Ok(Self {
            program,
            args,
            cwd: Some(cwd.into()),
            env,
            remove_env: Vec::new(),
            clear_env: true,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    pub pid: u32,
    pub child: ChildStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl ProcessOutput {
    pub fn success(&self) -> bool {
        self.child.code == Some(0) && self.child.signal.is_none() && !self.timed_out
    }

    pub fn mapped_exit(&self) -> i32 {
        self.child
            .code
            .or_else(|| self.child.signal.map(|signal| 128 + signal))
            .unwrap_or(1)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tee {
    Inherit,
    Stderr,
    Quiet,
}

/// Runs one captured subprocess with a bounded deadline.
pub fn capture(request: &CommandRequest, timeout: Duration) -> Result<ProcessOutput> {
    capture_op(request, timeout, None)
}

/// Runs one captured subprocess, naming the operation for the timing log.
/// Only a caller that composed the arguments itself may name them.
pub fn capture_op(
    request: &CommandRequest,
    timeout: Duration,
    op: Option<&str>,
) -> Result<ProcessOutput> {
    let traced = trace_spawn(request, op)?;
    let mut command = build_command(request, true);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let outcome = command
        .spawn()
        .map_err(|error| spawn_error(request, error))
        .and_then(|child| wait_with_pipes(child, timeout, None, Tee::Quiet, true));
    traced.finish(outcome_of(&outcome));
    outcome
}

fn outcome_of(outcome: &Result<ProcessOutput>) -> crate::trace::Outcome {
    match outcome {
        Err(_) => crate::trace::Outcome::Failed,
        Ok(output) if output.timed_out => crate::trace::Outcome::TimedOut,
        Ok(output) => output
            .child
            .code
            .map_or(crate::trace::Outcome::Failed, crate::trace::Outcome::Code),
    }
}

/// Runs a child while teeing byte-exact stdout/stderr to a log and a selected sink.
pub fn run(
    request: &CommandRequest,
    log: Option<&Path>,
    timeout: Option<Duration>,
    tee: Tee,
) -> Result<ProcessOutput> {
    run_with_header(request, log, None, timeout, tee)
}

/// Starts a process in a new session through an intermediate child, so the
/// launched process is reparented and outlives the invoking CLI.
pub fn spawn_detached(request: &CommandRequest) -> Result<u32> {
    let traced = trace_spawn(request, None)?;
    traced.finish(crate::trace::Outcome::Detached);
    let mut pipe = [0; 2];
    // SAFETY: `pipe` points to two writable integers.
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } < 0 {
        return Err(io_error("create detached-spawn pipe")(
            std::io::Error::last_os_error(),
        ));
    }
    for fd in pipe {
        // SAFETY: both descriptors were just created by `pipe`.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            // SAFETY: both descriptors remain owned by this process.
            unsafe {
                libc::close(pipe[0]);
                libc::close(pipe[1]);
            }
            return Err(io_error("mark detached-spawn pipe close-on-exec")(
                std::io::Error::last_os_error(),
            ));
        }
    }
    // SAFETY: fork duplicates this process. The child performs only the
    // bounded session/fork/exec sequence below and exits with `_exit`.
    let first = unsafe { libc::fork() };
    if first < 0 {
        // SAFETY: both descriptors were created by `pipe` and remain owned here.
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
        return Err(io_error("fork detached supervisor")(
            std::io::Error::last_os_error(),
        ));
    }
    if first == 0 {
        // SAFETY: the child owns its duplicated descriptors.
        unsafe { libc::close(pipe[0]) };
        if unsafe { libc::setsid() } < 0 {
            detached_child_error(pipe[1], "setsid failed");
        }
        // SAFETY: the second fork creates the reparented supervisor process.
        let second = unsafe { libc::fork() };
        if second < 0 {
            detached_child_error(pipe[1], "second fork failed");
        }
        if second > 0 {
            // SAFETY: the intermediate child must not run Rust destructors.
            unsafe { libc::_exit(0) };
        }
        let supervisor_pid = std::process::id();
        detached_child_write(pipe[1], format!("{supervisor_pid}\n").as_bytes());
        let mut command = build_command(request, false);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let error = command.exec();
        detached_child_error(pipe[1], &format!("exec failed: {error}"));
    }

    // SAFETY: the parent no longer writes to the exec-status pipe.
    unsafe { libc::close(pipe[1]) };
    let mut status = 0;
    // SAFETY: `first` is this process's direct child and `status` is writable.
    if unsafe { libc::waitpid(first, &mut status, 0) } < 0 {
        // SAFETY: close the still-owned read descriptor on the error path.
        unsafe { libc::close(pipe[0]) };
        return Err(io_error("wait for detached supervisor")(
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: the read descriptor is uniquely owned here and transferred to File.
    let mut reader = unsafe { File::from_raw_fd(pipe[0]) };
    let mut error = String::new();
    reader
        .read_to_string(&mut error)
        .map_err(io_error("read detached-spawn status"))?;
    let mut lines = error.lines();
    let supervisor_pid = lines.next().and_then(|line| line.parse::<u32>().ok());
    let child_error = lines.collect::<Vec<_>>().join("\n");
    if let Some(supervisor_pid) = supervisor_pid.filter(|_| child_error.is_empty()) {
        Ok(supervisor_pid)
    } else {
        Err(CoreError::new(
            ExitClass::External,
            "SPAWN_FAILED",
            format!(
                "could not spawn detached `{}`: {}",
                request.program.to_string_lossy(),
                if child_error.is_empty() {
                    error.trim()
                } else {
                    &child_error
                }
            ),
            "verify that the command is executable and retry",
        ))
    }
}

fn detached_child_error(fd: RawFd, message: &str) -> ! {
    detached_child_write(fd, message.as_bytes());
    // SAFETY: the forked child must not run Rust destructors.
    unsafe { libc::_exit(127) }
}

fn detached_child_write(fd: RawFd, bytes: &[u8]) {
    // SAFETY: `fd` is the child's live pipe descriptor and `bytes` is valid for
    // the duration of the write.
    unsafe {
        libc::write(fd, bytes.as_ptr().cast(), bytes.len());
    }
}

/// Reports whether a recorded process id still names a live process.
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs only an existence/permission check.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Runs a child like [`run`] and writes a header only to the task log.
pub fn run_with_header(
    request: &CommandRequest,
    log: Option<&Path>,
    log_header: Option<&[u8]>,
    timeout: Option<Duration>,
    tee: Tee,
) -> Result<ProcessOutput> {
    let traced = trace_spawn(request, None)?;
    let mut log = log.map(open_log).transpose()?;
    if let (Some(file), Some(header)) = (&mut log, log_header) {
        file.write_all(header)
            .map_err(io_error("write task log header"))?;
    }
    let process_group = !std::io::stdin().is_terminal();
    let mut command = build_command(request, process_group);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let outcome = command
        .spawn()
        .map_err(|error| spawn_error(request, error))
        .and_then(|child| {
            wait_with_pipes(
                child,
                timeout.unwrap_or(Duration::MAX),
                log,
                tee,
                process_group,
            )
        });
    traced.finish(outcome_of(&outcome));
    outcome
}

/// Runs a child with all three standard streams attached to one pseudoterminal.
/// Contract tests use this to exercise the real TTY consent boundary.
pub fn pty_status(request: &CommandRequest, input: &[u8]) -> Result<ChildStatus> {
    Ok(pty_capture(request, input, Duration::from_secs(30))?.child)
}

/// Runs a child on a pseudoterminal and captures the byte-exact combined stream.
pub fn pty_capture(
    request: &CommandRequest,
    input: &[u8],
    timeout: Duration,
) -> Result<ProcessOutput> {
    let traced = trace_spawn(request, None)?;
    let mut master_fd = 0;
    let mut slave_fd = 0;
    // SAFETY: openpty initialises two owned descriptors on success. Each is
    // immediately wrapped in File exactly once.
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Err(io_error("open pseudoterminal")(
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: openpty returned fresh owned descriptors.
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    // SAFETY: openpty returned fresh owned descriptors.
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let stdout = slave
        .try_clone()
        .map_err(io_error("clone pseudoterminal stdout"))?;
    let stderr = slave
        .try_clone()
        .map_err(io_error("clone pseudoterminal stderr"))?;
    let mut command = build_command(request, false);
    command
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // A terminal-facing program such as tmux needs a controlling terminal,
    // not merely terminal file descriptors. The test child owns the fresh PTY
    // slave after stdio setup, so it can safely become that terminal's session
    // leader without affecting the harness process.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::c_ulong::from(libc::TIOCSCTTY), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| spawn_error(request, error))?;
    let pid = child.id();
    if !input.is_empty() {
        master
            .write_all(input)
            .map_err(io_error("write pseudoterminal input"))?;
    }
    set_nonblocking(master.as_raw_fd())?;
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut status = None;
    let mut ended = false;
    let timed_out = loop {
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(io_error("wait for pseudoterminal child"))?;
        }
        drain_pty(&mut master, &mut bytes, &mut ended)?;
        if status.is_some() && ended {
            break false;
        }
        if status.is_some() {
            let before = bytes.len();
            poll_fd(master.as_raw_fd(), Duration::from_millis(5))?;
            drain_pty(&mut master, &mut bytes, &mut ended)?;
            if ended || bytes.len() == before {
                break false;
            }
        }
        if started.elapsed() >= timeout {
            if status.is_none() {
                terminate(&mut child, false)?;
                status = Some(
                    child
                        .wait()
                        .map_err(io_error("reap timed-out pseudoterminal child"))?,
                );
            }
            drain_pty(&mut master, &mut bytes, &mut ended)?;
            break true;
        }
        poll_fd(master.as_raw_fd(), Duration::from_millis(5))?;
    };
    let status = match status {
        Some(status) => status,
        None => child
            .wait()
            .map_err(io_error("wait for pseudoterminal child"))?,
    };
    let output = ProcessOutput {
        pid,
        child: ChildStatus {
            code: status.code(),
            signal: status.signal(),
        },
        stdout: bytes,
        stderr: Vec::new(),
        timed_out,
    };
    traced.finish(if timed_out {
        crate::trace::Outcome::TimedOut
    } else {
        output
            .child
            .code
            .map_or(crate::trace::Outcome::Failed, crate::trace::Outcome::Code)
    });
    Ok(output)
}

fn drain_pty(master: &mut File, bytes: &mut Vec<u8>, ended: &mut bool) -> Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match master.read(&mut buffer) {
            Ok(0) => {
                *ended = true;
                return Ok(());
            }
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            // Linux reports EIO when the final slave descriptor closes.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                *ended = true;
                return Ok(());
            }
            Err(error) => return Err(io_error("read pseudoterminal output")(error)),
        }
    }
}

fn poll_fd(fd: RawFd, timeout: Duration) -> Result<()> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: descriptor points to one initialised pollfd for the duration of the call.
    let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
    if result < 0 {
        Err(io_error("poll pseudoterminal")(
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

/// Maps the probe contract's 0/1/≥2 outcomes, including timeout and spawn failure.
pub fn probe(request: &CommandRequest, timeout: Duration, at: impl Into<String>) -> Probe {
    let at = at.into();
    match capture(request, timeout) {
        Ok(output) if output.timed_out => Probe::failed_timeout(at),
        Ok(output) => match output.child.code {
            Some(0) => Probe::present(at),
            Some(1) => Probe::absent(at),
            Some(code) if code >= 2 => Probe::failed_exit(at, code)
                .expect("an exit code of at least two satisfies the probe contract"),
            _ => Probe::failed_spawn(
                at,
                output.child.signal.map_or_else(
                    || "probe ended without a status".to_owned(),
                    |signal| format!("probe died from signal {signal}"),
                ),
            ),
        },
        Err(error) => Probe::failed_spawn(at, error.to_string()),
    }
}

/// Replaces the current process and leaves exactly `inherited_fd` non-cloexec.
pub fn execvp(request: &CommandRequest, inherited_fd: RawFd) -> Result<()> {
    execvp_inheriting(request, &[inherited_fd])
}

/// Replaces the process after clearing cloexec on every explicitly listed lock fd.
pub fn execvp_inheriting(request: &CommandRequest, inherited_fds: &[RawFd]) -> Result<()> {
    for inherited_fd in inherited_fds {
        clear_cloexec(*inherited_fd)?;
    }
    spawn_tracer(request)?;
    // The exec is the command's own ending, not a child of it.
    crate::trace::command_handoff(&request.program);
    let mut command = build_command(request, false);
    let error = command.exec();
    Err(spawn_error(request, error))
}

/// Appends one machine-readable spawn observation for contract budget tests.
/// Records the spawn for the acceptance tracer and starts the timing of this
/// child; every path that starts a process passes through here.
fn trace_spawn(request: &CommandRequest, op: Option<&str>) -> Result<crate::trace::Child> {
    let child = crate::trace::spawn(&request.program, op);
    spawn_tracer(request)?;
    Ok(child)
}

fn spawn_tracer(request: &CommandRequest) -> Result<()> {
    let Some(path) = std::env::var_os("WT_SPAWN_TRACE") else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| io_error("open spawn trace")(error))?;
    let record = serde_json::json!({
        "program": request.program.to_string_lossy(),
        "args": request.args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        "cwd": request.cwd.as_ref().map(|path| path.to_string_lossy()),
    });
    writeln!(file, "{record}").map_err(io_error("write spawn trace"))
}

fn build_command(request: &CommandRequest, process_group: bool) -> Command {
    let mut command = Command::new(&request.program);
    command.args(&request.args);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    if request.clear_env {
        command.env_clear();
    }
    for key in &request.remove_env {
        command.env_remove(key);
    }
    command.envs(&request.env);
    if process_group {
        // A group deadline also stops descendants holding the child's pipes.
        command.process_group(0);
    }
    command
}

fn wait_with_pipes(
    mut child: Child,
    timeout: Duration,
    mut log: Option<File>,
    tee: Tee,
    process_group: bool,
) -> Result<ProcessOutput> {
    let pid = child.id();
    let mut stdout = Some(child.stdout.take().ok_or_else(pipe_error)?);
    let mut stderr = Some(child.stderr.take().ok_or_else(pipe_error)?);
    set_nonblocking(stdout.as_ref().expect("stdout pipe exists").as_raw_fd())?;
    set_nonblocking(stderr.as_ref().expect("stderr pipe exists").as_raw_fd())?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let started = Instant::now();
    let mut status = None;
    let timed_out = loop {
        if status.is_none() {
            status = child.try_wait().map_err(io_error("wait for child"))?;
        }
        if status.is_some() && stdout.is_none() && stderr.is_none() {
            break false;
        }
        if started.elapsed() >= timeout {
            let pid = child_pid(&child)?;
            if status.is_some() {
                signal_target(pid, process_group, libc::SIGTERM)?;
            } else {
                terminate(&mut child, process_group)?;
                status = Some(child.wait().map_err(io_error("reap timed-out child"))?);
            }
            // Descendants can retain the write ends after the direct child exits.
            // Closing our read ends is what makes the overall wait deadline bounded.
            stdout.take();
            stderr.take();
            break true;
        }

        poll_pipes(stdout.as_ref(), stderr.as_ref(), Duration::from_millis(5))?;
        drain_available(&mut stdout, &mut stdout_bytes, &mut log, stdout_sink(tee))?;
        drain_available(&mut stderr, &mut stderr_bytes, &mut log, stderr_sink(tee))?;
    };

    let status = match status {
        Some(status) => status,
        None => child.wait().map_err(io_error("wait for child"))?,
    };
    Ok(ProcessOutput {
        pid,
        child: ChildStatus {
            code: status.code(),
            signal: status.signal(),
        },
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        timed_out,
    })
}

fn child_pid(child: &Child) -> Result<i32> {
    i32::try_from(child.id()).map_err(|_| {
        CoreError::new(
            ExitClass::Internal,
            "PROCESS_ID_INVALID",
            "child process id does not fit the platform pid type",
            "retry the command",
        )
    })
}

fn terminate(child: &mut Child, process_group: bool) -> Result<()> {
    let pid = child_pid(child)?;
    signal_target(pid, process_group, libc::SIGTERM)?;
    let until = Instant::now() + TERM_GRACE;
    while Instant::now() < until {
        if child
            .try_wait()
            .map_err(io_error("wait after SIGTERM"))?
            .is_some()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    signal_target(pid, process_group, libc::SIGKILL)
}

fn signal_target(pid: i32, process_group: bool, signal: i32) -> Result<()> {
    let target = if process_group { -pid } else { pid };
    // SAFETY: `target` is derived from the spawned child's pid and `signal` is
    // one of SIGTERM/SIGKILL. No pointer or ownership crosses the FFI boundary.
    let result = unsafe { libc::kill(target, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(io_error("signal child process")(error))
    }
}

#[derive(Clone, Copy)]
enum Sink {
    Stdout,
    Stderr,
    None,
}

fn stdout_sink(tee: Tee) -> Sink {
    match tee {
        Tee::Inherit => Sink::Stdout,
        Tee::Stderr => Sink::Stderr,
        Tee::Quiet => Sink::None,
    }
}

fn stderr_sink(tee: Tee) -> Sink {
    match tee {
        Tee::Inherit | Tee::Stderr => Sink::Stderr,
        Tee::Quiet => Sink::None,
    }
}

fn drain_available<R: Read>(
    source: &mut Option<R>,
    captured: &mut Vec<u8>,
    log: &mut Option<File>,
    sink: Sink,
) -> Result<()> {
    let Some(reader) = source.as_mut() else {
        return Ok(());
    };
    let mut buffer = [0_u8; 8192];
    // Return to the deadline loop regularly even when a child writes faster
    // than we can drain, so continuous output cannot starve timeout checks.
    for _ in 0..8 {
        let count = match reader.read(&mut buffer) {
            Ok(0) => {
                source.take();
                return Ok(());
            }
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error("read child output")(error)),
        };
        let bytes = &buffer[..count];
        captured.extend_from_slice(bytes);
        if let Some(file) = log {
            file.write_all(bytes).map_err(io_error("write task log"))?;
        }
        match sink {
            Sink::Stdout => std::io::stdout()
                .write_all(bytes)
                .map_err(io_error("tee child stdout"))?,
            Sink::Stderr => std::io::stderr()
                .write_all(bytes)
                .map_err(io_error("tee child stderr"))?,
            Sink::None => {}
        }
    }
    Ok(())
}

fn poll_pipes(
    stdout: Option<&ChildStdout>,
    stderr: Option<&ChildStderr>,
    wait: Duration,
) -> Result<()> {
    let mut descriptors = Vec::with_capacity(2);
    if let Some(stdout) = stdout {
        descriptors.push(libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if let Some(stderr) = stderr {
        descriptors.push(libc::pollfd {
            fd: stderr.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if descriptors.is_empty() {
        thread::sleep(wait);
        return Ok(());
    }
    let timeout = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `descriptors` is a valid mutable array for the supplied length;
    // poll only updates each element's `revents` field.
    let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(io_error("poll child output")(error));
        }
    }
    Ok(())
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
    // SAFETY: `fd` is an owned child-pipe descriptor and F_GETFL does not
    // transfer ownership or dereference memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io_error("read child pipe flags")(
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: the descriptor remains owned by its ChildStdout/ChildStderr;
    // this changes only its status flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io_error("set child pipe nonblocking")(
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn open_log(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error(&format!("open log {}", path.display())))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(io_error(&format!("set log mode {}", path.display())))?;
    Ok(file)
}

fn clear_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: `fd` is supplied by an fd-owning lock token and remains owned by
    // that token; fcntl only reads its descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io_error("read fd flags")(std::io::Error::last_os_error()));
    }
    // SAFETY: the same valid descriptor remains owned by its token; this only
    // updates FD_CLOEXEC so the requested lock survives exec.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io_error("clear FD_CLOEXEC")(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn spawn_error(request: &CommandRequest, error: std::io::Error) -> CoreError {
    CoreError::new(
        ExitClass::External,
        "SPAWN_FAILED",
        format!(
            "could not spawn `{}`: {error}",
            request.program.to_string_lossy()
        ),
        "install the command and verify that it is executable",
    )
}

fn io_error(context: &str) -> impl FnOnce(std::io::Error) -> CoreError + '_ {
    move |error| {
        CoreError::new(
            ExitClass::Internal,
            "IO_FAILED",
            format!("{context}: {error}"),
            "retry the operation; if it repeats, inspect filesystem and process permissions",
        )
    }
}

fn pipe_error() -> CoreError {
    CoreError::new(
        ExitClass::Internal,
        "PIPE_MISSING",
        "spawned child has no output pipe",
        "retry the operation",
    )
}

pub fn os_args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;
    use wt_core::resource::ProbeResult;

    use super::*;

    fn shell(script: &str) -> CommandRequest {
        CommandRequest {
            program: "sh".into(),
            args: os_args(&["-c", script]),
            cwd: None,
            env: BTreeMap::new(),
            remove_env: Vec::new(),
            clear_env: false,
        }
    }

    #[test]
    fn capture_preserves_binary_output_and_maps_signal_exit() {
        let output = capture(
            &shell("printf '\\377x'; printf err >&2; exit 7"),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(output.stdout, [0xff, b'x']);
        assert_eq!(output.stderr, b"err");
        assert_eq!(output.mapped_exit(), 7);
        let signaled = capture(&shell("kill -TERM $$"), Duration::from_secs(1)).unwrap();
        assert_eq!(signaled.child.signal, Some(libc::SIGTERM));
        assert_eq!(signaled.mapped_exit(), 128 + libc::SIGTERM);
    }

    #[test]
    fn run_tees_both_streams_to_a_mode_0600_log() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let log = dir.path().join("task.log");
        let output = run(
            &shell("printf out; printf err >&2"),
            Some(&log),
            Some(Duration::from_secs(1)),
            Tee::Quiet,
        )
        .unwrap();
        assert!(output.success());
        let bytes = std::fs::read(&log).unwrap();
        assert!(bytes.windows(3).any(|part| part == b"out"));
        assert!(bytes.windows(3).any(|part| part == b"err"));
        assert_eq!(
            std::fs::metadata(log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn probe_maps_absent_infrastructure_failure_timeout_and_spawn() {
        assert_eq!(
            probe(&shell("exit 1"), Duration::from_secs(1), "a").result,
            ProbeResult::Absent
        );
        assert!(matches!(
            probe(&shell("exit 2"), Duration::from_secs(1), "b").result,
            ProbeResult::Failed { .. }
        ));
        let started = Instant::now();
        assert!(matches!(
            probe(&shell("sleep 1"), Duration::from_millis(10), "c").result,
            ProbeResult::Failed { .. }
        ));
        assert!(started.elapsed() < Duration::from_millis(500));
        let missing = CommandRequest::new("definitely-no-such-wt-test-program");
        assert!(matches!(
            probe(&missing, Duration::from_millis(10), "d").result,
            ProbeResult::Failed { .. }
        ));
    }

    #[test]
    fn deadline_bounds_pipe_drain_after_the_direct_child_exits() {
        let request = shell("(sleep 6; echo late) & exit 0");
        for operation in ["capture", "run", "probe"] {
            let started = Instant::now();
            match operation {
                "capture" => assert!(
                    capture(&request, Duration::from_millis(500))
                        .unwrap()
                        .timed_out
                ),
                "run" => assert!(
                    run(&request, None, Some(Duration::from_millis(500)), Tee::Quiet,)
                        .unwrap()
                        .timed_out
                ),
                "probe" => assert!(matches!(
                    probe(&request, Duration::from_millis(500), "now").result,
                    ProbeResult::Failed { .. }
                )),
                _ => unreachable!(),
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{operation} exceeded its bounded drain deadline"
            );
        }
    }
}
