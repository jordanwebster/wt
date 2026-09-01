//! Small machine questions `setup` asks before it proposes anything
//! (A76, §14.7).

use std::time::Duration;

use wt_core::setup::{PackageManager, PACKAGE_MANAGERS};

use crate::proc::{self, CommandRequest};

/// Whether this machine's terminfo database knows an entry.
///
/// Naming a `default-terminal` that terminfo does not have makes tmux refuse
/// to start, so the generated configuration asks before it names one.
pub fn has_terminfo(entry: &str, deadline: Duration) -> bool {
    let Some(program) = proc::on_path("infocmp") else {
        // Without infocmp there is no way to know; the conservative entry is
        // the one every curses installation has carried for decades.
        return false;
    };
    let mut request = CommandRequest::new(program);
    request.args = proc::os_args(&["-1", entry]);
    proc::capture(&request, deadline)
        .map(|output| output.success())
        .unwrap_or(false)
}

/// The first package manager on this machine wt knows how to install through.
pub fn package_manager() -> Option<&'static PackageManager> {
    PACKAGE_MANAGERS
        .iter()
        .find(|manager| proc::on_path(manager.program).is_some())
}

/// Whether a program is installed.
pub fn installed(program: &str) -> bool {
    proc::on_path(program).is_some()
}

/// The program that invoked wt, which is the shell a person is actually
/// typing into.
///
/// `$SHELL` is the login shell from the password database, which is not
/// necessarily the one running now — someone who logs in with bash and works
/// in fish would otherwise be offered the wrong file.
pub fn parent_program(deadline: Duration) -> Option<String> {
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        return None;
    }
    let program = proc::on_path("ps")?;
    let mut request = CommandRequest::new(program);
    request.args = proc::os_args(&["-o", "comm=", "-p", &parent.to_string()]);
    let output = proc::capture(&request, deadline).ok()?;
    if !output.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    // A login shell is reported with a leading dash.
    let name = name.trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name).to_owned();
    (!name.is_empty()).then_some(name)
}

/// The shell to install into: the invoking program when it is one wt knows,
/// else `$SHELL`.
pub fn detected_shell(deadline: Duration) -> Option<String> {
    let known = |name: String| {
        wt_core::setup::SHELLS
            .contains(&name.as_str())
            .then_some(name)
    };
    parent_program(deadline).and_then(known).or_else(|| {
        std::env::var("SHELL")
            .ok()
            .and_then(|shell| shell.rsplit('/').next().map(str::to_owned))
            .and_then(known)
    })
}
