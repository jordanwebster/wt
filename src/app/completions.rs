use wt_core::report::ScriptData;
use wt_core::CoreError;

use crate::cli::Script;

use super::{Context, Output};

pub(crate) fn run(_context: &mut Context, args: Script) -> Result<Output, CoreError> {
    generate(args.shell)
}

pub(crate) fn generate(shell: crate::cli::Shell) -> Result<Output, CoreError> {
    let script = match shell {
        crate::cli::Shell::Bash => {
            r#"_wt_targets() {
  COMPREPLY=( $(compgen -W "$(command wt list --json | tr '{' '\n' | sed -n 's/.*\"target\":\"\([^\"]*\)\".*/\1/p')" -- "${COMP_WORDS[COMP_CWORD]}") )
}
complete -F _wt_targets wt"#
        }
        crate::cli::Shell::Zsh => {
            r#"#compdef wt
_wt() {
  local -a targets
  targets=("${(@f)$(command wt list --json | tr '{' '\n' | sed -n 's/.*\"target\":\"\([^\"]*\)\".*/\1/p')}")
  _describe 'wt target' targets
}
compdef _wt wt"#
        }
        crate::cli::Shell::Fish => {
            r#"function __wt_targets
    command wt list --json | string split '{' | string match -rg '"target":"([^"]*)"'
end
complete -c wt -a '(__wt_targets)'"#
        }
    };
    Output::text(
        ScriptData {
            shell: shell.as_str().to_owned(),
            script: script.to_owned(),
        },
        script,
    )
}
