//! Thin, effectful platform integration for `wt`.
// The workspace-root lint blocks effects in the binary; this crate is their
// sole implementation boundary (SPEC §15).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod failpoint;
pub mod forge;
pub mod fsx;
pub mod git;
pub mod lock;
pub mod net;
pub mod probe;
pub mod proc;
pub mod snapshot;
pub mod term;
pub mod tmux;
pub mod trace;
pub mod walk;

pub type Result<T> = std::result::Result<T, wt_core::CoreError>;

#[cfg(test)]
pub(crate) mod stub {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;

    /// Writes a shell stub that is safe to execute immediately.
    ///
    /// A file that any process still holds open for writing cannot be
    /// executed (`ETXTBSY`). These tests run in parallel, and a sibling
    /// thread that spawns a process between this write and the exec under
    /// test inherits the write descriptor for as long as it takes that child
    /// to exec, which makes the stub briefly unexecutable and fails the test
    /// for a reason that has nothing to do with what it is proving. Running
    /// the stub once here — with its body skipped, so no side effect the test
    /// asserts on can happen — waits that window out before the test begins.
    pub fn write(path: &Path, script: &str) {
        let (shebang, body) = script.split_once('\n').expect("stub needs a shebang");
        let guarded = format!("{shebang}\n[ -n \"${{WT_STUB_WARMUP:-}}\" ] && exit 0\n{body}");
        std::fs::write(path, guarded).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        settle(path);
    }

    fn settle(path: &Path) {
        // ETXTBSY is 26 on every platform this runs on; std does not name it
        // below the 1.83 `ErrorKind::ExecutableFileBusy` this crate predates.
        const ETXTBSY: i32 = 26;
        for _ in 0..200 {
            match std::process::Command::new(path)
                .env("WT_STUB_WARMUP", "1")
                .output()
            {
                Err(error) if error.raw_os_error() == Some(ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => return,
            }
        }
    }
}
