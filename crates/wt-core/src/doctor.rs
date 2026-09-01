use serde::{Deserialize, Serialize};

use crate::{adapters::AdapterContribution, model::EnvMap, template, CoreError};
use std::collections::BTreeSet;

pub const CODES: &[&str] = &[
    "STATE_ORPHAN",
    "CACHE_ORPHAN",
    "REPO_PATH_MISSING",
    "TREE_REPLACED",
    "TREE_MISSING",
    "TREE_INCOMPLETE",
    "TREE_INTERRUPTED",
    "INIT_INTERRUPTED",
    "REMOVE_INTERRUPTED",
    "TREE_CLAIMED",
    "VERIFY_PENDING",
    "UNMANAGED_WORKTREE",
    "STALE_GIT_WORKTREE",
    "BRANCH_MERGED",
    "UPSTREAM_GONE",
    "RESOURCE_ORPHANED",
    "RESOURCE_GONE",
    "RESOURCE_UNDECLARED",
    "RESOURCE_PROBE_FAILED",
    "REFRESH_SKIPPED",
    "NAME_MAY_COLLIDE",
    "TREE_MISSING_PENDING",
    "GEOMETRY_CHANGED",
    "SLOT_SQUATTED",
    "PORT_SQUATTED",
    "PORTS_EXHAUSTED",
    "ADAPTER_TOOL_MISSING",
    "ACCELERATOR_INACTIVE",
    "ACCELERATOR_AVAILABLE",
    "ACCELERATOR_MISSING",
    "NO_LOCKFILE",
    "NO_ADAPTER",
    "NO_VERIFY",
    "NO_COORDINATION",
    "SESSION_BACKEND",
    "SHELL_INIT_MISSING",
    "BIN_DIR_MISSING",
    "SHIM_BROKEN",
    "SHIM_SHADOWED",
    "PATH_NOT_SHADOWED",
    "PORT_BOUND",
    "EXCLUDE_MISSING",
    "EXCLUDE_REPAIRED",
    "ACTIVATION_IGNORED",
    "IDENTIFIER_LONG",
    "TREE_IN_USE",
    "GIT_TOO_OLD",
];

pub fn is_code(code: &str) -> bool {
    CODES.contains(&code)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warn,
    Info,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    pub subject: String,
    pub message: String,
    pub remedy: String,
}

impl Finding {
    pub fn new(
        severity: Severity,
        code: impl Into<String>,
        subject: impl Into<String>,
        message: impl Into<String>,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code: code.into(),
            subject: subject.into(),
            message: message.into(),
            remedy: remedy.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingCounts {
    pub error: usize,
    pub warn: usize,
    pub info: usize,
}

pub fn sort_and_count(findings: &mut [Finding]) -> FindingCounts {
    findings.sort_by(|left, right| {
        (left.severity, &left.code, &left.subject).cmp(&(
            right.severity,
            &right.code,
            &right.subject,
        ))
    });
    let mut counts = FindingCounts::default();
    for finding in findings {
        match finding.severity {
            Severity::Error => counts.error += 1,
            Severity::Warn => counts.warn += 1,
            Severity::Info => counts.info += 1,
        }
    }
    counts
}

pub fn resource_name_findings(
    subject: &str,
    name_template: Option<&str>,
    expanded_name: &str,
) -> Result<Vec<Finding>, CoreError> {
    let mut findings = Vec::new();
    if expanded_name.len() > 63 {
        findings.push(Finding::new(
            Severity::Info,
            "IDENTIFIER_LONG",
            subject,
            "resource name is longer than 63 characters",
            "shorten the resource name template",
        ));
    }
    if let Some(name_template) = name_template {
        let calls = template::calls(name_template)?;
        let uses_ambiguous = calls.iter().any(|call| {
            matches!(call, template::Call::Simple(name) if name == "name" || name == "name_snake")
        });
        let uses_identity = calls.iter().any(|call| {
            matches!(call, template::Call::Simple(name) if name == "name_short" || name == "root" || name == "target")
        });
        if uses_ambiguous && !uses_identity {
            findings.push(Finding::new(
                Severity::Info,
                "NAME_MAY_COLLIDE",
                subject,
                "resource name uses a many-to-one tree name without an allocated identity",
                "include name_short(), target(), or root() in the name",
            ));
        }
    }
    Ok(findings)
}

pub fn adapter_findings(
    subject: &str,
    contribution: &AdapterContribution,
    available_binaries: &BTreeSet<String>,
    effective_env: &EnvMap,
    machine_files: &std::collections::BTreeMap<String, String>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for requirement in &contribution.requirements {
        if !available_binaries.contains(requirement) {
            findings.push(Finding::new(
                Severity::Warn,
                "ADAPTER_TOOL_MISSING",
                subject,
                format!("adapter requires `{requirement}`"),
                format!("install `{requirement}` or disable the adapter"),
            ));
        }
    }
    for nudge in &contribution.nudges {
        if nudge
            .if_tool
            .as_ref()
            .is_some_and(|tool| !contribution.selected_tools.contains(tool))
        {
            continue;
        }
        let active = nudge.used_if_env.iter().any(|assignment| {
            assignment
                .split_once('=')
                .is_some_and(|(key, value)| effective_env.get(key).is_some_and(|set| set == value))
        }) || nudge
            .used_if_file
            .iter()
            .any(|sniff| crate::adapters::sniff_content_matches(sniff, machine_files));
        let available = available_binaries.contains(&nudge.want);
        if active && available {
            continue;
        }
        if available && !(nudge.used_if_env.is_empty() && nudge.used_if_file.is_empty()) {
            findings.push(Finding::new(
                Severity::Warn,
                "ACCELERATOR_INACTIVE",
                subject,
                format!("accelerator `{}` is available but inactive", nudge.want),
                nudge.hint.clone(),
            ));
        } else if available {
            findings.push(Finding::new(
                Severity::Info,
                "ACCELERATOR_AVAILABLE",
                subject,
                format!("accelerator `{}` is available", nudge.want),
                nudge.hint.clone(),
            ));
        } else {
            findings.push(Finding::new(
                Severity::Info,
                "ACCELERATOR_MISSING",
                subject,
                format!("optional accelerator `{}` is unavailable", nudge.want),
                nudge.hint.clone(),
            ));
        }
    }
    findings
}

pub fn state_orphan(subject: &str, path: &str) -> Finding {
    Finding::new(
        Severity::Info,
        "STATE_ORPHAN",
        subject,
        format!("state file `{path}` has no live registry entry"),
        "run `wt prune` to delete the orphaned state file",
    )
}

pub fn cache_orphan(subject: &str, path: &str) -> Finding {
    Finding::new(
        Severity::Info,
        "CACHE_ORPHAN",
        subject,
        format!("cache path `{path}` belongs to no live worktree"),
        "run `wt prune` to delete the orphaned cache",
    )
}

pub fn no_coordination(
    label: &str,
    has_ports: bool,
    has_env_aliases: bool,
    has_resources: bool,
) -> Option<Finding> {
    (!has_ports && !has_env_aliases && !has_resources).then(|| {
        Finding::new(
            Severity::Info,
            "NO_COORDINATION",
            label,
            "parallel trees share the application's default coordinates",
            "declare `ports`/`env` in `.wt.toml` or `$WT_HOME/config.toml [repos.<label>]`",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::Nudge;

    #[test]
    fn pure_doctor_findings_cover_resource_names_and_adapter_metadata() {
        let findings = resource_name_findings(
            "db",
            Some("aspire-{{label()}}-{{name_snake()}}"),
            &"x".repeat(64),
        )
        .unwrap();
        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.code.as_str())
                .collect::<Vec<_>>(),
            ["IDENTIFIER_LONG", "NAME_MAY_COLLIDE"]
        );

        let contribution = AdapterContribution {
            requirements: vec!["cargo".to_owned()],
            nudges: vec![Nudge {
                if_tool: None,
                want: "sccache".to_owned(),
                hint: "install sccache".to_owned(),
                used_if_env: vec!["RUSTC_WRAPPER=sccache".to_owned()],
                used_if_file: vec![crate::adapters::Sniff {
                    file: "~/.cargo/config.toml".to_owned(),
                    toml_key: Some("build.rustc-wrapper".to_owned()),
                    contains: Some("sccache".to_owned()),
                }],
            }],
            selected_tools: BTreeSet::from(["cargo".to_owned()]),
            ..AdapterContribution::default()
        };
        let findings = adapter_findings(
            "repo",
            &contribution,
            &BTreeSet::from(["sccache".to_owned()]),
            &EnvMap::new(),
            &std::collections::BTreeMap::new(),
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "ADAPTER_TOOL_MISSING"));
        assert!(findings
            .iter()
            .any(|finding| finding.code == "ACCELERATOR_INACTIVE"));

        // A machine config file that names the wrapper activates the nudge
        // exactly as the environment assignment would.
        let via_file = adapter_findings(
            "repo",
            &contribution,
            &BTreeSet::from(["cargo".to_owned(), "sccache".to_owned()]),
            &EnvMap::new(),
            &std::collections::BTreeMap::from([(
                "~/.cargo/config.toml".to_owned(),
                "[build]\nrustc-wrapper = \"sccache\"\n".to_owned(),
            )]),
        );
        assert!(!via_file
            .iter()
            .any(|finding| finding.code.starts_with("ACCELERATOR_")));

        let missing = adapter_findings(
            "repo",
            &contribution,
            &BTreeSet::new(),
            &EnvMap::from([("RUSTC_WRAPPER".to_owned(), "sccache".to_owned())]),
            &std::collections::BTreeMap::new(),
        );
        assert!(missing
            .iter()
            .any(|finding| finding.code == "ACCELERATOR_MISSING"));

        let available = AdapterContribution {
            nudges: vec![Nudge {
                if_tool: Some("npm".to_owned()),
                want: "pnpm".to_owned(),
                hint: "use pnpm".to_owned(),
                used_if_env: Vec::new(),
                used_if_file: Vec::new(),
            }],
            selected_tools: BTreeSet::from(["npm".to_owned()]),
            ..AdapterContribution::default()
        };
        assert!(adapter_findings(
            "repo",
            &available,
            &BTreeSet::from(["pnpm".to_owned()]),
            &EnvMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .iter()
        .any(|finding| finding.code == "ACCELERATOR_AVAILABLE"));

        let wrong_tool = AdapterContribution {
            nudges: vec![Nudge {
                if_tool: Some("npm".to_owned()),
                want: "pnpm".to_owned(),
                hint: "use pnpm".to_owned(),
                used_if_env: Vec::new(),
                used_if_file: Vec::new(),
            }],
            selected_tools: BTreeSet::from(["pnpm".to_owned()]),
            ..AdapterContribution::default()
        };
        assert!(adapter_findings(
            "repo",
            &wrong_tool,
            &BTreeSet::from(["pnpm".to_owned()]),
            &EnvMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .is_empty());
        assert!(CODES.iter().all(|code| is_code(code)));
        assert_eq!(
            state_orphan("repo/work", "/state/repo/work.json").severity,
            Severity::Info
        );
        assert!(no_coordination("repo", false, false, false).is_some());
        assert!(no_coordination("repo", true, false, false).is_none());
    }
}
