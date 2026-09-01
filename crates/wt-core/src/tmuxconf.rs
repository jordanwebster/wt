//! Generation and difference reporting for tmux configuration (A76, §14.7).
//!
//! The configuration is generated rather than shipped because two of its lines
//! cannot be written ahead of time: `terminal-features` has to name the
//! terminal wt is actually running under, and `tmux-256color` is absent from
//! some systems' terminfo, where naming it stops tmux starting at all.

use serde::{Deserialize, Serialize};

/// The marker opening a block wt owns inside a file it did not write.
pub const BLOCK_OPEN: &str = "# >>> wt >>>";
/// The marker closing a block wt owns.
pub const BLOCK_CLOSE: &str = "# <<< wt <<<";

/// `TERM` kept alongside the detected one, so an ssh'd session that reports
/// the portable name still gets colour and modified keys.
const PORTABLE_TERM: &str = "xterm-256color";

/// What the machine says about tmux, gathered before anything is written.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TmuxObs {
    /// The outer terminal's `TERM`; `setup` runs outside tmux, so this is the
    /// value `terminal-features` has to describe.
    pub term: Option<String>,
    /// Whether `tmux-256color` exists in this machine's terminfo.
    pub tmux_256color: bool,
}

impl TmuxObs {
    /// The `default-terminal` this machine can actually use.
    ///
    /// Naming a terminfo entry that does not exist makes tmux refuse to start,
    /// which is why this is a probe rather than a constant.
    pub fn default_terminal(&self) -> &'static str {
        if self.tmux_256color {
            "tmux-256color"
        } else {
            "screen-256color"
        }
    }

    /// The terminals `terminal-features` will describe, most specific first.
    pub fn described_terms(&self) -> Vec<String> {
        let mut terms = Vec::new();
        if let Some(term) = self.term.as_deref() {
            let term = term.trim();
            if !term.is_empty() && term != "dumb" {
                terms.push(term.to_owned());
            }
        }
        if !terms.iter().any(|term| term == PORTABLE_TERM) {
            terms.push(PORTABLE_TERM.to_owned());
        }
        terms
    }

    fn features(&self, feature: &str) -> String {
        let entries = self
            .described_terms()
            .into_iter()
            .map(|term| format!("{term}:{feature}"))
            .collect::<Vec<_>>()
            .join(",");
        format!(",{entries}")
    }

    /// The `terminal-features` value that grants true colour.
    pub fn rgb_features(&self) -> String {
        self.features("RGB")
    }

    /// The `terminal-features` value that grants modified keys.
    pub fn extkeys_features(&self) -> String {
        self.features("extkeys")
    }
}

/// Renders the configuration wt writes when a machine has none.
pub fn render(obs: &TmuxObs) -> String {
    format!(
        r#"# colour — inside tmux TERM must be a tmux terminfo entry, so programs do
# not think they are in a plain xterm; terminal-features describes the OUTER
# terminal, so it names the $TERM detected when this was written.
set  -g  default-terminal "{terminal}"
set  -as terminal-features "{rgb}"

# modified keys — without these, shift+enter does not reach a coding agent
# running in a pane.
set  -s  extended-keys on
set  -as terminal-features "{extkeys}"

# mouse — this takes over selection; hold shift to select text natively.
set  -g  mouse on

# prefix — this costs the shell's ctrl+a (beginning-of-line).
unbind   C-b
set  -g  prefix C-a
bind     C-a send-prefix

# windows start at 1 and stay contiguous, so prefix n/p stay predictable.
set  -g  base-index 1
set  -g  renumber-windows on

set  -sg escape-time 10
set  -g  history-limit 50000
set  -g  focus-events on
setw -g  aggressive-resize on
set  -g  set-clipboard on
"#,
        terminal = obs.default_terminal(),
        rgb = obs.rgb_features(),
        extkeys = obs.extkeys_features(),
    )
}

/// The options a probe of an existing configuration reports.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Effective {
    pub default_terminal: Option<String>,
    pub extended_keys: Option<String>,
    pub terminal_features: Option<String>,
    pub mouse: Option<String>,
}

/// One option an existing configuration does not set the way wt needs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub option: String,
    pub current: String,
    pub line: String,
    /// What breaks while the option stands as it is.
    pub consequence: String,
}

/// Reports only the differences wt actually requires.
///
/// Prefix, indices, history and split keys are the author's taste, and a setup
/// command has no business having an opinion about a configuration somebody
/// already wrote. For the same reason `terminal-features` is judged for the
/// terminal wt is running under only: the generated file also names a
/// portable fallback for ssh'd sessions, but a configuration that covers
/// this terminal works here, and an existing file's silence about a terminal
/// its author does not use is not a fault.
pub fn deltas(obs: &TmuxObs, effective: &Effective) -> Vec<Delta> {
    let mut deltas = Vec::new();
    let terminal = effective.default_terminal.as_deref().unwrap_or("");
    if !terminal.contains("256color") {
        deltas.push(Delta {
            option: "default-terminal".to_owned(),
            current: display(effective.default_terminal.as_deref()),
            line: format!("set -g default-terminal \"{}\"", obs.default_terminal()),
            consequence: "programs inside tmux fall back to 8 colours".to_owned(),
        });
    }
    if !matches!(effective.extended_keys.as_deref(), Some("on" | "always")) {
        deltas.push(Delta {
            option: "extended-keys".to_owned(),
            current: display(effective.extended_keys.as_deref()),
            line: "set -s extended-keys on".to_owned(),
            consequence: "shift+enter does not reach an agent running in a pane".to_owned(),
        });
    }
    let features = effective.terminal_features.as_deref().unwrap_or("");
    let uncovered_rgb = uncovered(obs, features, "RGB");
    if !uncovered_rgb.is_empty() {
        deltas.push(Delta {
            option: "terminal-features".to_owned(),
            current: format!("no RGB for {}", uncovered_rgb.join(", ")),
            line: format!("set -as terminal-features \"{}\"", obs.rgb_features()),
            consequence: "true colour is approximated to the 256-colour palette".to_owned(),
        });
    }
    let uncovered_keys = uncovered(obs, features, "extkeys");
    if !uncovered_keys.is_empty() {
        deltas.push(Delta {
            option: "terminal-features".to_owned(),
            current: format!("no extkeys for {}", uncovered_keys.join(", ")),
            line: format!("set -as terminal-features \"{}\"", obs.extkeys_features()),
            consequence: "tmux never asks the terminal for modified keys".to_owned(),
        });
    }
    if effective.mouse.as_deref() != Some("on") {
        deltas.push(Delta {
            option: "mouse".to_owned(),
            current: display(effective.mouse.as_deref()),
            line: "set -g mouse on".to_owned(),
            consequence: "panes cannot be selected or resized with the mouse".to_owned(),
        });
    }
    deltas
}

fn display(value: Option<&str>) -> String {
    match value {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => "unset".to_owned(),
    }
}

/// The current terminal, when `features` does not grant it `feature`. Empty
/// when there is no terminal to judge.
fn uncovered(obs: &TmuxObs, features: &str, feature: &str) -> Vec<String> {
    obs.described_terms()
        .into_iter()
        .take(1)
        .filter(|term| term != PORTABLE_TERM || obs.term.as_deref() == Some(PORTABLE_TERM))
        .filter(|term| !covers(features, term, feature))
        .collect()
}

/// Whether a `terminal-features` value grants `feature` to `term`.
fn covers(features: &str, term: &str, feature: &str) -> bool {
    features.split(',').any(|entry| {
        let mut parts = entry.split(':');
        let Some(pattern) = parts.next() else {
            return false;
        };
        matches_pattern(pattern.trim(), term) && parts.any(|part| part.trim() == feature)
    })
}

/// tmux terminal patterns are shell globs; only `*` appears in practice.
fn matches_pattern(pattern: &str, term: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if pattern == "*" {
        return true;
    }
    let mut rest = term;
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return false;
    };
    if !rest.starts_with(first) {
        return false;
    }
    rest = &rest[first.len()..];
    let mut trailing = None;
    for segment in segments {
        trailing = Some(segment);
        if segment.is_empty() {
            continue;
        }
        match rest.find(segment) {
            Some(index) => rest = &rest[index + segment.len()..],
            None => return false,
        }
    }
    match trailing {
        // No `*` at all: the whole pattern had to match exactly.
        None => rest.is_empty(),
        // A trailing literal has to end the term; a trailing `*` takes the rest.
        Some(segment) if !segment.is_empty() => term.ends_with(segment),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(term: &str, terminfo: bool) -> TmuxObs {
        TmuxObs {
            term: Some(term.to_owned()),
            tmux_256color: terminfo,
        }
    }

    #[test]
    fn a_missing_terminfo_entry_downgrades_the_default_terminal() {
        assert_eq!(
            obs("xterm-ghostty", true).default_terminal(),
            "tmux-256color"
        );
        assert_eq!(
            obs("xterm-ghostty", false).default_terminal(),
            "screen-256color"
        );
    }

    #[test]
    fn features_name_the_detected_terminal_and_the_portable_one() {
        let obs = obs("xterm-ghostty", true);
        assert_eq!(obs.rgb_features(), ",xterm-ghostty:RGB,xterm-256color:RGB");
        assert_eq!(
            obs.extkeys_features(),
            ",xterm-ghostty:extkeys,xterm-256color:extkeys"
        );
    }

    #[test]
    fn the_portable_terminal_is_not_named_twice() {
        let obs = obs("xterm-256color", true);
        assert_eq!(obs.rgb_features(), ",xterm-256color:RGB");
    }

    #[test]
    fn an_absent_or_dumb_term_still_describes_the_portable_one() {
        let absent = TmuxObs {
            term: None,
            tmux_256color: true,
        };
        assert_eq!(absent.described_terms(), vec!["xterm-256color".to_owned()]);
        assert_eq!(obs("dumb", true).described_terms(), vec!["xterm-256color"]);
    }

    #[test]
    fn a_generated_config_names_the_probed_values() {
        let rendered = render(&obs("xterm-ghostty", true));
        assert!(rendered.contains("set  -g  default-terminal \"tmux-256color\""));
        assert!(rendered.contains(",xterm-ghostty:RGB,xterm-256color:RGB"));
        assert!(rendered.contains(",xterm-ghostty:extkeys,xterm-256color:extkeys"));
        assert!(rendered.contains("set  -s  extended-keys on"));
        assert!(rendered.contains("set  -g  mouse on"));
        assert!(rendered.contains("set  -g  prefix C-a"));
    }

    #[test]
    fn a_satisfied_config_produces_no_deltas() {
        let obs = obs("xterm-ghostty", true);
        let effective = Effective {
            default_terminal: Some("tmux-256color".to_owned()),
            extended_keys: Some("on".to_owned()),
            terminal_features: Some(
                "xterm-ghostty:RGB:extkeys,xterm-256color:RGB:extkeys".to_owned(),
            ),
            mouse: Some("on".to_owned()),
        };
        assert_eq!(deltas(&obs, &effective), Vec::new());
    }

    #[test]
    fn an_empty_config_reports_every_requirement_once() {
        let deltas = deltas(&obs("xterm-ghostty", true), &Effective::default());
        let options = deltas
            .iter()
            .map(|delta| delta.option.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            options,
            vec![
                "default-terminal",
                "extended-keys",
                "terminal-features",
                "terminal-features",
                "mouse"
            ]
        );
    }

    #[test]
    fn deltas_are_limited_to_what_wt_requires() {
        // A config with an opinionated prefix and indices is left alone.
        let effective = Effective {
            default_terminal: Some("tmux-256color".to_owned()),
            extended_keys: Some("always".to_owned()),
            terminal_features: Some("*:RGB:extkeys".to_owned()),
            mouse: Some("on".to_owned()),
        };
        assert!(deltas(&obs("xterm-ghostty", true), &effective).is_empty());
    }

    #[test]
    fn a_wildcard_pattern_covers_every_terminal() {
        assert!(covers("*:RGB", "xterm-ghostty", "RGB"));
        assert!(covers(",*256col*:RGB", "xterm-256color", "RGB"));
        assert!(!covers(",*256col*:RGB", "xterm-ghostty", "RGB"));
    }

    #[test]
    fn coverage_needs_the_named_feature_not_merely_the_terminal() {
        assert!(!covers("xterm-ghostty:RGB", "xterm-ghostty", "extkeys"));
        assert!(covers(
            "xterm-ghostty:RGB:extkeys",
            "xterm-ghostty",
            "extkeys"
        ));
    }

    #[test]
    fn patterns_anchor_at_both_ends() {
        assert!(matches_pattern("xterm-ghostty", "xterm-ghostty"));
        assert!(!matches_pattern("xterm", "xterm-ghostty"));
        assert!(matches_pattern("xterm*", "xterm-ghostty"));
        assert!(matches_pattern("*ghostty", "xterm-ghostty"));
        assert!(!matches_pattern("*ghost", "xterm-ghostty"));
    }

    #[test]
    fn a_config_covering_this_terminal_is_not_faulted_for_the_portable_one() {
        // The generated file names both, but an existing configuration that
        // works under the terminal in use has nothing wt needs to add.
        let effective = Effective {
            default_terminal: Some("tmux-256color".to_owned()),
            extended_keys: Some("on".to_owned()),
            terminal_features: Some("xterm-ghostty:RGB:extkeys".to_owned()),
            mouse: Some("on".to_owned()),
        };
        assert_eq!(deltas(&obs("xterm-ghostty", true), &effective), Vec::new());
        // And the reverse: covering only the portable name is a fault here.
        let portable_only = Effective {
            terminal_features: Some("xterm-256color:RGB:extkeys".to_owned()),
            ..effective
        };
        let reported = deltas(&obs("xterm-ghostty", true), &portable_only);
        assert_eq!(reported.len(), 2, "{reported:?}");
        assert!(reported
            .iter()
            .all(|delta| delta.current.contains("ghostty")));
        // With no terminal to judge, nothing about features is claimed.
        let blind = TmuxObs {
            term: None,
            tmux_256color: true,
        };
        assert!(deltas(&blind, &portable_only).is_empty());
    }

    #[test]
    fn a_partial_config_reports_only_what_is_missing() {
        let effective = Effective {
            default_terminal: Some("tmux-256color".to_owned()),
            extended_keys: Some("off".to_owned()),
            terminal_features: Some("xterm-ghostty:RGB,xterm-256color:RGB".to_owned()),
            mouse: Some("on".to_owned()),
        };
        let deltas = deltas(&obs("xterm-ghostty", true), &effective);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].option, "extended-keys");
        assert_eq!(deltas[0].current, "off");
        assert_eq!(deltas[1].option, "terminal-features");
        assert!(deltas[1].current.contains("extkeys"));
    }
}
