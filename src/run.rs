//! Running a tree's hooks the way every copy of wt agrees on: one at a time
//! under the tree's lifecycle lock, in a process group of their own, and
//! killed when they run past their time limit.

use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::error::{Error, Result, io_at};
use crate::{Hook, Tree};

/// Called while wt waits on a lock or a hook. A signal it returns is passed
/// on to the hook's process group, or, while still waiting for the lock,
/// gives up with [`Error::Cancelled`]. For a command-line program, whose
/// terminal no longer reaches a hook in a process group of its own.
pub type Forward = fn() -> Option<i32>;

/// Where a hook's stdout and stderr go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HookOutput {
    /// `.wt/<hook>.log` in the tree, replaced on each run. For `create`, the
    /// same `.wt/create.log` as always.
    #[default]
    Log,
    /// This process's own stdout and stderr. For `create`, also copied to
    /// `.wt/create.log`.
    Inherit,
    Null,
}

/// How to run a hook.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunHook {
    pub output: HookOutput,
    pub forward: Option<Forward>,
}

/// How a hook run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The config doesn't declare the hook, so there was nothing to run.
    NotDeclared,
    Succeeded,
    Failed(i32),
    /// Killed by this signal, from outside wt.
    Killed(i32),
    /// Ran past its time limit, so wt killed its process group.
    TimedOut(Duration),
    /// Ended after wt passed it this signal from [`RunHook::forward`]: the
    /// person asked to give up.
    Interrupted(i32),
}

/// A hook wt ran.
#[derive(Clone, Debug)]
pub struct Ran {
    /// The tree's `label/name`.
    pub tree: String,
    pub hook: Hook,
    pub outcome: Outcome,
    /// Where its output went, when that was a file.
    pub log: Option<PathBuf>,
}

impl Ran {
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome, Outcome::NotDeclared | Outcome::Succeeded)
    }

    /// What went wrong, for a person, or `None` if nothing did.
    pub fn problem(&self) -> Option<String> {
        let what = format!("{}'s {} hook", self.tree, self.hook.name());
        let problem = match &self.outcome {
            Outcome::NotDeclared | Outcome::Succeeded => return None,
            Outcome::Failed(code) => format!("{what} failed (exit {code})"),
            Outcome::Killed(signal) => format!("{what} was killed by signal {signal}"),
            Outcome::TimedOut(limit) => format!(
                "{what} ran past its time limit of {} and was killed",
                span(*limit)
            ),
            Outcome::Interrupted(signal) => format!("{what} was interrupted (signal {signal})"),
        };
        Some(match &self.log {
            Some(log) => format!("{problem}; its output is in {}", log.display()),
            None => problem,
        })
    }

    /// Like [`Ran::problem`], with the end of the hook's log in place of its
    /// path, for a log about to be removed with its tree.
    pub(crate) fn problem_with_output(&self) -> Option<String> {
        let problem = Ran {
            log: None,
            ..self.clone()
        }
        .problem()?;
        let tail = self
            .log
            .as_ref()
            .and_then(|log| fs::read_to_string(log).ok())
            .map(|text| {
                let lines: Vec<&str> = text.lines().collect();
                lines[lines.len().saturating_sub(10)..].join("\n")
            })
            .filter(|tail| !tail.trim().is_empty());
        Some(match tail {
            Some(tail) => format!("{problem}; it printed:\n{tail}"),
            None => problem,
        })
    }

    /// The exit code a command-line program passes on for this run.
    pub fn exit_code(&self) -> u8 {
        match self.outcome {
            Outcome::NotDeclared | Outcome::Succeeded => 0,
            Outcome::Failed(code) => code.clamp(1, 255) as u8,
            Outcome::Killed(signal) | Outcome::Interrupted(signal) => {
                (128 + signal).clamp(1, 255) as u8
            }
            Outcome::TimedOut(_) => 124,
        }
    }
}

/// How long a hook past its time limit gets between SIGTERM and SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// How often a [`Forward`] is asked for a signal.
const FORWARD_POLL: Duration = Duration::from_millis(100);

/// The file descriptors a hook's wrapper holds the lifecycle lock and the
/// tree's exclusive hold on, so they last until the hook ends even if the
/// process that ran it dies. Single digits, because POSIX `sh` only promises
/// `9>&-` for those.
pub(crate) const LIFECYCLE_FD: i32 = 9;
pub(crate) const HOLDS_FD: i32 = 8;

impl Tree {
    /// Runs `hook` once no other hook of this tree is running, and waits for
    /// it. `start`, `stop` and `delete` run in a process group of their own,
    /// with stdin closed, and are killed with their group after the time
    /// limit from `[lifecycle]` (5 minutes unless it says otherwise).
    /// `create` has no limit and records its progress as [`Tree::hook`]
    /// describes.
    pub fn run_hook(&self, hook: Hook, options: RunHook) -> Result<Ran> {
        let lock = self.lock_lifecycle(options.forward)?;
        let ran = self.run_locked(hook, options, &lock, None);
        let _ = lock.unlock();
        ran
    }

    /// Runs `hook` while the caller holds the lifecycle lock in `lock`, and,
    /// for a stop or a removal, the exclusive hold in `holds`. The hook's
    /// wrapper keeps both until the hook ends, even if this process dies
    /// first; the hook itself, and whatever it leaves running, never has
    /// them.
    pub(crate) fn run_locked(
        &self,
        hook: Hook,
        options: RunHook,
        lock: &File,
        holds: Option<&File>,
    ) -> Result<Ran> {
        let config = self.config()?;
        let tee = options.output == HookOutput::Inherit;
        let Some(mut command) = self.hook_command(hook, tee, &[])? else {
            return Ok(self.ran(hook, Outcome::NotDeclared, None));
        };
        let log = self.direct_output(&mut command, hook, options.output)?;
        pass_locks(&mut command, lock, holds);
        // In a process group of its own the hook is in the terminal's
        // background, where reading the terminal would stop it.
        command.stdin(Stdio::null()).process_group(0);
        // A start, or a create run again, may bring services up.
        if matches!(hook, Hook::Start | Hook::Create) {
            self.set_stopped(false)?;
        }
        let child = command.spawn().map_err(io_at(
            &format!("running the {} hook of", hook.name()),
            &self.root,
        ))?;
        let outcome = wait(child, config.lifecycle.time_limit(hook), options.forward)?;
        if hook == Hook::Stop {
            // Only a stop that succeeded says the tree is stopped; one that
            // didn't leaves it in a state nobody knows.
            if outcome == Outcome::Succeeded {
                self.mark_stopped_unless_held(holds)?;
            } else {
                self.set_stopped(false)?;
            }
        }
        Ok(self.ran(hook, outcome, log))
    }

    fn ran(&self, hook: Hook, outcome: Outcome, log: Option<PathBuf>) -> Ran {
        Ran {
            tree: self.id(),
            hook,
            outcome,
            log,
        }
    }

    fn direct_output(
        &self,
        command: &mut Command,
        hook: Hook,
        output: HookOutput,
    ) -> Result<Option<PathBuf>> {
        if hook == Hook::Create {
            // The wrapper writes create.log itself.
            if output != HookOutput::Inherit {
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
            return Ok(Some(self.create_log()));
        }
        match output {
            HookOutput::Inherit => Ok(None),
            HookOutput::Null => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
                Ok(None)
            }
            HookOutput::Log => {
                let path = self.root.join(format!(".wt/{}.log", hook.name()));
                // A file rather than a pipe: something the hook leaves
                // running with its output open can't keep wt waiting.
                let file = File::create(&path).map_err(io_at("creating", &path))?;
                let stderr = file.try_clone().map_err(io_at("opening", &path))?;
                command.stdout(file).stderr(stderr);
                Ok(Some(path))
            }
        }
    }
}

/// Gives the child the lifecycle lock as fd 9 and the exclusive hold, if
/// any, as fd 8, which it inherits across exec: a lock lasts while any
/// process has its descriptor open.
pub(crate) fn pass_locks(command: &mut Command, lifecycle: &File, holds: Option<&File>) {
    let fds = [
        Some((lifecycle.as_raw_fd(), LIFECYCLE_FD)),
        holds.map(|holds| (holds.as_raw_fd(), HOLDS_FD)),
    ];
    // SAFETY: fcntl and dup2 are async-signal-safe, and the closure touches
    // nothing but its integers.
    unsafe {
        command.pre_exec(move || {
            // Each to a close-on-exec copy above 9 first, so that moving one
            // into place can't overwrite another still to be moved.
            let mut copies = [-1; 2];
            for (copy, fds) in copies.iter_mut().zip(fds) {
                if let Some((from, _)) = fds {
                    *copy = libc::fcntl(from, libc::F_DUPFD_CLOEXEC, 10);
                    if *copy < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
            }
            for (copy, fds) in copies.into_iter().zip(fds) {
                if let Some((_, to)) = fds
                    && libc::dup2(copy, to) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Waits for `child`, the leader of its own process group, killing the group
/// if it runs past `limit` and passing on what `forward` returns.
fn wait(child: Child, limit: Option<Duration>, forward: Option<Forward>) -> Result<Outcome> {
    let group = child.id() as i32;
    let (sender, receiver) = mpsc::channel();
    let mut child = child;
    std::thread::spawn(move || {
        let _ = sender.send(child.wait());
    });
    let deadline = limit.map(|limit| Instant::now() + limit);
    let mut forwarded = None;
    let outcome =
        |status: std::io::Result<ExitStatus>, forwarded: Option<i32>| -> Result<Outcome> {
            let status = status.map_err(|source| Error::Io {
                context: "waiting for a hook".to_owned(),
                source,
            })?;
            if let Some(signal) = forwarded {
                return Ok(Outcome::Interrupted(signal));
            }
            Ok(match (status.code(), status.signal()) {
                (Some(0), _) => Outcome::Succeeded,
                (Some(code), _) => Outcome::Failed(code),
                (None, Some(signal)) => Outcome::Killed(signal),
                (None, None) => Outcome::Failed(-1),
            })
        };
    loop {
        let remaining = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        let slice = match (remaining, forward) {
            (Some(remaining), Some(_)) => remaining.min(FORWARD_POLL),
            (Some(remaining), None) => remaining,
            (None, Some(_)) => FORWARD_POLL,
            (None, None) => return outcome(receiver.recv().expect("the waiter sends"), None),
        };
        match receiver.recv_timeout(slice) {
            Ok(status) => return outcome(status, forwarded),
            Err(RecvTimeoutError::Disconnected) => unreachable!("the waiter sends"),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(signal) = forward.and_then(|forward| forward()) {
            forwarded = Some(signal);
            // SAFETY: a plain syscall on our own child's process group.
            unsafe { libc::kill(-group, signal) };
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            // SAFETY: as above. The group outlives its leader only while
            // members remain, and the leader hasn't been reaped yet.
            unsafe { libc::kill(-group, libc::SIGTERM) };
            if receiver.recv_timeout(KILL_GRACE).is_err() {
                unsafe { libc::kill(-group, libc::SIGKILL) };
                let _ = receiver.recv();
            } else {
                // The leader is gone; whatever it left in the group goes too.
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            return Ok(Outcome::TimedOut(limit.expect("a deadline has a limit")));
        }
    }
}

/// Opens (creating it) a lock file inside the tree's `.wt`, never creating
/// the tree itself: a tree removed meanwhile is [`Error::NotFound`].
pub(crate) fn open_lock(tree: &Tree, name: &str) -> Result<File> {
    let gone = || Error::NotFound(format!("{} was removed", tree.id()));
    if !tree.root.is_dir() {
        return Err(gone());
    }
    let directory = tree.root.join(".wt");
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(gone()),
        Err(error) => return Err(io_at("creating", &directory)(error)),
    }
    let path = directory.join(name);
    match File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(gone()),
        Err(error) => Err(io_at("opening", &path)(error)),
    }
}

/// Waits for `file`'s lock. A signal that interrupts the wait and that
/// `forward` reports gives up with [`Error::Cancelled`].
pub(crate) fn wait_for_lock(
    file: &File,
    shared: bool,
    forward: Option<Forward>,
    path: &std::path::Path,
) -> Result<()> {
    // A signal that arrived just before the wait wouldn't interrupt it.
    if forward.and_then(|forward| forward()).is_some() {
        return Err(Error::Cancelled);
    }
    loop {
        let result = if shared {
            file.lock_shared()
        } else {
            file.lock()
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                if forward.and_then(|forward| forward()).is_some() {
                    return Err(Error::Cancelled);
                }
            }
            Err(error) => return Err(io_at("locking", path)(error)),
        }
    }
}

/// `90s`, `5m`, `2h`.
fn span(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        _ if duration.subsec_millis() != 0 && secs < 60 => format!("{}ms", duration.as_millis()),
        0..120 => format!("{secs}s"),
        _ if secs.is_multiple_of(3600) => format!("{}h", secs / 3600),
        _ if secs.is_multiple_of(60) => format!("{}m", secs / 60),
        _ => format!("{secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(span(Duration::from_secs(300)), "5m");
        assert_eq!(span(Duration::from_secs(1)), "1s");
        assert_eq!(span(Duration::from_millis(1500)), "1500ms");
        assert_eq!(span(Duration::from_secs(7200)), "2h");
    }
}
