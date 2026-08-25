use wt_core::report::ScriptData;
use wt_core::CoreError;

use crate::cli::{Script, Shell};

use super::{Context, Output};

pub(crate) fn run(_context: &mut Context, args: Script) -> Result<Output, CoreError> {
    generate(args.shell)
}

pub(crate) fn generate(shell: Shell) -> Result<Output, CoreError> {
    let script = script(shell);
    Output::text(
        ScriptData {
            shell: shell.as_str().to_owned(),
            script: script.to_owned(),
        },
        script,
    )
}

pub(crate) fn script(shell: Shell) -> &'static str {
    match shell {
        Shell::Zsh | Shell::Bash => {
            r#"wtcd() { local p; p="$(command wt path -- "$@")" || return $?; builtin cd -- "$p"; }
wtsh() { eval "$(command wt env --sh -- "$@")"; }
if [ -n "$WT_PATH_PREFIX" ] && [ "${PATH#"$WT_PATH_PREFIX:"}" = "$PATH" ]; then PATH="$WT_PATH_PREFIX:$PATH"; export PATH; echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_PATH_PREFIX" >&2; fi"#
        }
        Shell::Fish => {
            r#"function wtcd; set -l p (command wt path -- $argv); or return $status; cd -- $p; end
function wtsh; command wt env --sh -- $argv | source; end
if set -q WT_PATH_PREFIX
    set -l wt_prefix (string split : -- $WT_PATH_PREFIX)
    set -l n (count $wt_prefix)
    if test (count $PATH) -lt $n; or test "$(string join : -- $PATH[1..$n])" != "$(string join : -- $wt_prefix)"
        set -gx PATH $wt_prefix $PATH
        echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_PATH_PREFIX" >&2
    end
end"#
        }
    }
}
