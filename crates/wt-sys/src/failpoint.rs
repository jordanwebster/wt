//! Crash injection shared by lifecycle effect call sites.

use crate::Result;

#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Trips a named failpoint when the `failpoints` feature or unit-test cfg is active.
///
/// Production builds compile this to a no-op. `:exit` and `:sigkill` terminate,
/// `:pause=<ms>` delays and continues, and a bare match returns an error.
fn check(name: &str) -> Result<()> {
    check_enabled(name)
}

pub fn new_g() -> Result<()> {
    check("new.G")
}

pub fn sync_mid() -> Result<()> {
    check("sync.mid")
}

pub fn remove_8() -> Result<()> {
    check("remove.8")
}

pub fn render_write() -> Result<()> {
    check("render.write")
}

pub fn resource_destroyed() -> Result<()> {
    check("resource.destroyed")
}

pub fn resource_frozen() -> Result<()> {
    check("resource.frozen")
}

#[cfg(any(test, feature = "failpoints"))]
fn check_enabled(name: &str) -> Result<()> {
    use std::time::Duration;
    use wt_core::{CoreError, ExitClass};

    let Ok(value) = std::env::var("WT_FAILPOINT") else {
        return Ok(());
    };
    if std::env::var("WT_FAILPOINT_THREAD")
        .ok()
        .is_some_and(|thread| thread != format!("{:?}", std::thread::current().id()))
    {
        return Ok(());
    }
    let Some(action) = value
        .strip_prefix(name)
        .filter(|tail| tail.is_empty() || tail.starts_with(':'))
    else {
        return Ok(());
    };
    if action == ":sigkill" {
        // Failpoint subprocesses deliberately die without unwinding so crash
        // tests exercise the bytes that reached the filesystem (SPEC §17).
        // SAFETY: raising SIGKILL in the current process is the intentional
        // crash-test behavior and touches no Rust-owned memory.
        unsafe {
            libc::raise(libc::SIGKILL);
        }
    }
    if action == ":exit" {
        // SAFETY: `_exit` deliberately skips destructors to emulate a crash.
        unsafe {
            libc::_exit(86);
        }
    }
    if let Some(ms) = action.strip_prefix(":pause=") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::sleep(Duration::from_millis(ms));
        }
        return Ok(());
    }
    Err(CoreError::new(
        ExitClass::Internal,
        "FAILPOINT",
        format!("failpoint `{name}` tripped"),
        "disable WT_FAILPOINT and retry the interrupted operation",
    ))
}

#[cfg(not(any(test, feature = "failpoints")))]
fn check_enabled(_name: &str) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_named_point_trips() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var(
            "WT_FAILPOINT_THREAD",
            format!("{:?}", std::thread::current().id()),
        );
        for (name, point) in [
            ("new.G", new_g as fn() -> Result<()>),
            ("sync.mid", sync_mid),
            ("remove.8", remove_8),
            ("render.write", render_write),
            ("resource.destroyed", resource_destroyed),
            ("resource.frozen", resource_frozen),
        ] {
            std::env::set_var("WT_FAILPOINT", name);
            assert!(point().is_err());
        }
        assert!(check("not.selected").is_ok());
        std::env::remove_var("WT_FAILPOINT");
        std::env::remove_var("WT_FAILPOINT_THREAD");
    }
}
