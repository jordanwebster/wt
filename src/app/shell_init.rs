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
            r#"if [ -n "$WT_PATH_PREFIX" ] && [ "${PATH#"$WT_PATH_PREFIX:"}" = "$PATH" ]; then PATH="$WT_PATH_PREFIX:$PATH"; export PATH; echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_PATH_PREFIX" >&2; fi
if [ -n "${WT_TARGET:-}" ]; then case "${PS1:-}" in "($WT_TARGET) "*) ;; *) PS1="($WT_TARGET) ${PS1:-}" ;; esac; fi"#
        }
        Shell::Fish => {
            r#"if set -q WT_PATH_PREFIX
    set -l wt_prefix (string split : -- $WT_PATH_PREFIX)
    set -l n (count $wt_prefix)
    if test (count $PATH) -lt $n; or test "$(string join : -- $PATH[1..$n])" != "$(string join : -- $wt_prefix)"
        set -gx PATH $wt_prefix $PATH
        echo "wt: PATH_NOT_SHADOWED — re-prepended $WT_PATH_PREFIX" >&2
    end
end
if set -q WT_TARGET; and functions -q fish_prompt; and not functions -q __wt_original_fish_prompt
    functions -c fish_prompt __wt_original_fish_prompt
    function fish_prompt
        printf '(%s) ' "$WT_TARGET"
        __wt_original_fish_prompt
    end
end"#
        }
    }
}
