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
    trace_spawn(request)?;
    let mut command = build_command(request, true);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|error| spawn_error(request, error))?;
    wait_with_pipes(child, timeout, None, Tee::Quiet, true)
}

/// Runs a child while teeing byte-exact stdout/stderr to a log and a selected sink.
pub fn run(
    request: &CommandRequest,
    log: Option<&Path>,
    timeout: Option<Duration>,
    tee: Tee,
) -> Result<ProcessOutput> {
    trace_spawn(request)?;
    let log = log.map(open_log).transpose()?;
    let process_group = !std::io::stdin().is_terminal();
    let mut command = build_command(request, process_group);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|error| spawn_error(request, error))?;
    wait_with_pipes(
        child,
        timeout.unwrap_or(Duration::MAX),
        log,
        tee,
        process_group,
    )
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
    trace_spawn(request)?;
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
    Ok(ProcessOutput {
        child: ChildStatus {
            code: status.code(),
            signal: status.signal(),
        },
        stdout: bytes,
        stderr: Vec::new(),
        timed_out,
    })
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
    trace_spawn(request)?;
    let mut command = build_command(request, false);
    let error = command.exec();
    Err(spawn_error(request, error))
}

/// Appends one machine-readable spawn observation for contract budget tests.
fn trace_spawn(request: &CommandRequest) -> Result<()> {
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
