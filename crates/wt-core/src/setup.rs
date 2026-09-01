//! Pure decisions for `wt setup` (A76, §14.7).
//!
//! Everything here maps observations the walk and the git batch produced onto
//! the register/adopt operations `setup` will propose. It performs no effect
//! and reads no clock: recency arrives as a timestamp and a window.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Days within which a checkout counts as recent enough to auto-select.
pub const RECENT_DAYS: u64 = 28;

const SECONDS_PER_DAY: u64 = 86_400;

/// How a candidate directory relates to git.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// `.git` is a directory: an ordinary checkout.
    Checkout,
    /// `.git` is a file: a linked worktree or a submodule.
    Linked,
}

/// A linked worktree of some checkout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeObs {
    pub path: String,
    pub branch: Option<String>,
    pub touched: u64,
    /// Already a tree in the registry.
    pub registered: bool,
}

/// One checkout, after its single batch of git queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutObs {
    pub path: String,
    pub common_gitdir: String,
    pub origin: Option<String>,
    pub touched: u64,
    pub worktrees: Vec<WorktreeObs>,
    /// The label this checkout is already registered under.
    pub registered: Option<String>,
    /// Tree names already live under that label. A proposal that collides
    /// with one addresses the existing tree instead of the path offered.
    pub taken_names: Vec<String>,
}

/// A checkout as `setup` will show and act on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutRow {
    pub path: String,
    pub common_gitdir: String,
    pub label: String,
    pub touched: u64,
    pub registered: Option<String>,
    pub selected: bool,
    /// Set when this is not the first checkout of its origin, which is what
    /// makes it a second label rather than a second worktree.
    pub secondary: bool,
    pub worktrees: Vec<WorktreeRow>,
}

/// A linked worktree as `setup` will show and act on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeRow {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    pub touched: u64,
    pub registered: bool,
    pub selected: bool,
}

/// One origin's checkouts, in the order they are offered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bucket {
    /// Normalised `host/org/name`, or the directory name for an originless
    /// checkout.
    pub origin: String,
    pub has_origin: bool,
    pub checkouts: Vec<CheckoutRow>,
}

impl Bucket {
    /// The most recent activity anywhere in the bucket, which is how buckets
    /// sort.
    pub fn touched(&self) -> u64 {
        self.checkouts
            .iter()
            .map(|checkout| checkout.touched)
            .max()
            .unwrap_or(0)
    }

    /// True when nothing in the bucket has been touched inside the window, so
    /// the bucket belongs in the collapsed tail.
    pub fn stale(&self, now: u64, window_days: u64) -> bool {
        !recent(self.touched(), now, window_days)
    }
}

/// Whether a timestamp falls inside the recency window.
pub fn recent(touched: u64, now: u64, window_days: u64) -> bool {
    now.saturating_sub(touched) <= window_days.saturating_mul(SECONDS_PER_DAY)
}

/// Reduces a remote URL to `host/org/name`, so the several spellings of one
/// remote bucket together.
///
/// `git@github.com:acme/api.git`, `https://github.com/acme/api` and
/// `ssh://git@github.com/acme/api.git` all key to `github.com/acme/api`.
pub fn origin_key(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let (schemed, rest) = match url.split_once("://") {
        // `file://` names a path, not a host, so it keys like the bare path
        // it is: two checkouts of one local origin belong in one bucket.
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("file") => (false, rest),
        Some((_scheme, rest)) => (true, rest),
        None => (false, url),
    };
    let rest = rest.split_once('@').map_or(rest, |(_user, rest)| rest);
    // A colon means a port under a scheme (`host:22/path`) and a path
    // separator without one (scp-style `host:org/repo`); reading it the wrong
    // way silently drops the organisation and splits one origin into two.
    let (host, path) = if schemed {
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next().unwrap_or(host), path)
    } else if let Some((host, path)) = rest.split_once(':') {
        (host, path)
    } else {
        rest.trim_start_matches('/').split_once('/')?
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!(
        "{}/{}",
        host.to_ascii_lowercase(),
        path.to_ascii_lowercase()
    ))
}

/// Whether a remote URL names a directory on this machine rather than a host.
///
/// A local origin is usually a bare mirror named for convenience
/// (`~/repos/api-origin.git`), so the checkout's own directory is the better
/// label; the origin still groups checkouts of it together.
pub fn local_origin(url: &str) -> bool {
    let url = url.trim();
    url.starts_with('/')
        || url.starts_with("./")
        || url.starts_with("../")
        || url.starts_with("~/")
        || url
            .split_once("://")
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("file"))
}

/// The last component of a path, which is the name a reader knows it by.
pub fn basename_of(path: &str) -> Option<&str> {
    basename(path)
}

/// The repository name a label is proposed from.
fn origin_name(origin: &str) -> &str {
    origin.rsplit('/').next().unwrap_or(origin)
}

/// The organisation a colliding label is disambiguated with.
fn origin_org(origin: &str) -> Option<&str> {
    let mut parts = origin.rsplit('/');
    parts.next()?;
    parts.next()
}

/// Coerces arbitrary text into something `Label::new` accepts, or `None` when
/// nothing usable survives.
pub fn sanitise_label(value: &str) -> Option<String> {
    sanitise(value, 32)
}

/// Coerces arbitrary text into something a tree name accepts.
pub fn sanitise_name(value: &str) -> Option<String> {
    let value = sanitise(value, 64)?;
    if value == "canonical" {
        return Some(format!("{value}-tree"));
    }
    Some(value)
}

fn sanitise(value: &str, max: usize) -> Option<String> {
    let mut out = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            out.push(character.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches(|c| c == '-' || c == '.').to_owned();
    let out: String = out
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .collect();
    if out.is_empty() {
        return None;
    }
    let mut out = out;
    out.truncate(max);
    let out = out.trim_end_matches(['-', '.', '_']).to_owned();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Proposes a label for one checkout, avoiding everything already taken.
///
/// The origin's repository name is preferred over the directory name because
/// directory names collide across `~/work/api` and `~/oss/api` while origins
/// carry the organisation that tells them apart.
pub fn propose_label(origin: Option<&str>, path: &str, taken: &BTreeSet<String>) -> String {
    let mut candidates = Vec::new();
    if let Some(origin) = origin {
        if let Some(name) = sanitise_label(origin_name(origin)) {
            candidates.push(name.clone());
            if let Some(org) = origin_org(origin).and_then(sanitise_label) {
                candidates.push(truncate_label(&format!("{org}-{name}")));
            }
        }
    }
    if let Some(name) = basename(path).and_then(sanitise_label) {
        candidates.push(name);
    }
    for candidate in &candidates {
        if !taken.contains(candidate) {
            return candidate.clone();
        }
    }
    let stem = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| "repo".to_owned());
    for suffix in 2..1000 {
        let candidate = suffixed(&stem, suffix, 32);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    stem
}

fn truncate_label(value: &str) -> String {
    let mut value = value.to_owned();
    value.truncate(32);
    value.trim_end_matches(['-', '.', '_']).to_owned()
}

/// Proposes a tree name for a linked worktree, from its branch where it has
/// one — that is how `wt new` names trees — else its directory.
pub fn propose_name(branch: Option<&str>, path: &str, taken: &BTreeSet<String>) -> String {
    let mut candidates = Vec::new();
    if let Some(branch) = branch {
        // A branch's last segment is the part a person would say out loud.
        if let Some(name) = sanitise_name(branch.rsplit('/').next().unwrap_or(branch)) {
            candidates.push(name);
        }
        if let Some(name) = sanitise_name(branch) {
            candidates.push(name);
        }
    }
    if let Some(name) = basename(path).and_then(sanitise_name) {
        candidates.push(name);
    }
    for candidate in &candidates {
        if !taken.contains(candidate) {
            return candidate.clone();
        }
    }
    let stem = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| "tree".to_owned());
    for suffix in 2..1000 {
        let candidate = suffixed(&stem, suffix, 64);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    stem
}

/// `stem-suffix`, shortened from the stem so it fits `max`: truncating
/// afterwards would cut the suffix off a maximum-length name and return the
/// same taken identifier every time round.
fn suffixed(stem: &str, suffix: usize, max: usize) -> String {
    let tail = format!("-{suffix}");
    let mut head = stem.to_owned();
    head.truncate(max - tail.len());
    format!("{}{tail}", head.trim_end_matches(['-', '.', '_']))
}

fn basename(path: &str) -> Option<&str> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
}

/// Groups observed checkouts into origin buckets with labels and names
/// proposed, most recently touched first, nothing selected.
///
/// `registered_labels` are the labels the registry already holds, so a
/// proposal never collides with one.
pub fn bucket(checkouts: Vec<CheckoutObs>, registered_labels: &BTreeSet<String>) -> Vec<Bucket> {
    let mut grouped: BTreeMap<String, (bool, Vec<CheckoutObs>)> = BTreeMap::new();
    for checkout in checkouts {
        let (key, has_origin) = match checkout.origin.as_deref().and_then(origin_key) {
            Some(origin) => (origin, true),
            // An originless checkout buckets alone, keyed by its own path so
            // two of them never merge.
            None => (format!("\u{7f}{}", checkout.path), false),
        };
        let entry = grouped.entry(key).or_insert((has_origin, Vec::new()));
        entry.0 = has_origin;
        entry.1.push(checkout);
    }

    let mut taken = registered_labels.clone();
    let mut buckets = Vec::new();
    for (key, (has_origin, mut checkouts)) in grouped {
        // Most recent first, so the auto-selected checkout is the head and the
        // ordering two runs produce on one machine agrees.
        checkouts.sort_by(|left, right| {
            right
                .touched
                .cmp(&left.touched)
                .then_with(|| left.path.len().cmp(&right.path.len()))
                .then_with(|| left.path.cmp(&right.path))
        });
        let origin = if has_origin {
            key.clone()
        } else {
            basename(&checkouts[0].path).unwrap_or("repo").to_owned()
        };
        let local = checkouts
            .iter()
            .any(|checkout| checkout.origin.as_deref().is_some_and(local_origin));
        let mut rows = Vec::new();
        for (index, checkout) in checkouts.into_iter().enumerate() {
            let label = match &checkout.registered {
                Some(label) => label.clone(),
                None => {
                    let label = propose_label(
                        (has_origin && !local).then_some(origin.as_str()),
                        &checkout.path,
                        &taken,
                    );
                    taken.insert(label.clone());
                    label
                }
            };
            let secondary = index > 0;
            // Only the most recent checkout of an origin is proposed. A second
            // one can only become a second label, which is always deliberate.
            // Nothing is ticked on the reader's behalf: registering is opt
            // in, and a wrong tick that slips through costs a registration
            // they did not want. Recency orders the list and collapses its
            // stale tail; it decides nothing.
            let selected = false;
            let mut names = BTreeSet::from(["canonical".to_owned()]);
            names.extend(checkout.taken_names.iter().cloned());
            let mut worktrees = Vec::new();
            for worktree in &checkout.worktrees {
                let name = propose_name(worktree.branch.as_deref(), &worktree.path, &names);
                names.insert(name.clone());
                worktrees.push(WorktreeRow {
                    path: worktree.path.clone(),
                    name,
                    branch: worktree.branch.clone(),
                    touched: worktree.touched,
                    registered: worktree.registered,
                    selected: false,
                });
            }
            worktrees.sort_by(|left, right| {
                right
                    .touched
                    .cmp(&left.touched)
                    .then_with(|| left.path.cmp(&right.path))
            });
            rows.push(CheckoutRow {
                path: checkout.path,
                common_gitdir: checkout.common_gitdir,
                label,
                touched: checkout.touched,
                registered: checkout.registered,
                selected,
                secondary,
                worktrees,
            });
        }
        buckets.push(Bucket {
            origin,
            has_origin,
            checkouts: rows,
        });
    }
    buckets.sort_by(|left, right| {
        right
            .touched()
            .cmp(&left.touched())
            .then_with(|| left.origin.cmp(&right.origin))
    });
    buckets
}

/// The marker opening a block wt owns inside a file it did not write.
pub const BLOCK_OPEN: &str = "# >>> wt >>>";
/// The marker closing a block wt owns.
pub const BLOCK_CLOSE: &str = "# <<< wt <<<";

/// The shells `setup` can install into.
pub const SHELLS: &[&str] = &["zsh", "bash", "fish"];

/// The rc file, relative to the home directory, that a shell reads for the
/// interactive shells wt's doors start.
///
/// bash gets `.bashrc` rather than `.bash_profile` because a door shell is
/// interactive and non-login (§9.3), and `.bashrc` is the file bash reads for
/// exactly those.
pub fn rc_file(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some(".zshrc"),
        "bash" => Some(".bashrc"),
        "fish" => Some(".config/fish/config.fish"),
        _ => None,
    }
}

/// The block `setup` appends to an rc file.
pub fn shell_block(shell: &str) -> String {
    let body = if shell == "fish" {
        format!("wt shell-init {shell} | source\nwt completions {shell} | source")
    } else {
        format!("eval \"$(wt shell-init {shell})\"\neval \"$(wt completions {shell})\"")
    };
    format!("{BLOCK_OPEN}\n{body}\n{BLOCK_CLOSE}\n")
}

/// Whether an rc file already installs the guard.
///
/// Matching on the command rather than on wt's own markers means a line the
/// user wrote by hand, or moved out of the block, still counts — appending a
/// second copy would be the visible failure. A commented line does not count;
/// anything else that names the command is taken at its word, because a
/// guarded form such as `command -v wt >/dev/null && eval "$(wt shell-init
/// zsh)"` is exactly what a careful person writes.
pub fn block_installed(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line.contains("wt shell-init")
    })
}

/// A package manager `setup` knows how to install through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageManager {
    pub program: &'static str,
    pub argv: &'static [&'static str],
    /// Whether the manager needs root. Never true on macOS: Homebrew under
    /// `sudo` corrupts its own prefix.
    pub sudo: bool,
}

/// The managers, in the order they are looked for.
///
/// Homebrew leads because a machine that has it is a machine whose owner
/// installs through it, including on Linux.
pub const PACKAGE_MANAGERS: &[PackageManager] = &[
    PackageManager {
        program: "brew",
        argv: &["install"],
        sudo: false,
    },
    PackageManager {
        program: "apt-get",
        argv: &["install", "-y"],
        sudo: true,
    },
    PackageManager {
        program: "dnf",
        argv: &["install", "-y"],
        sudo: true,
    },
    PackageManager {
        program: "pacman",
        argv: &["-S", "--noconfirm"],
        sudo: true,
    },
    PackageManager {
        program: "zypper",
        argv: &["install", "-y"],
        sudo: true,
    },
    PackageManager {
        program: "apk",
        argv: &["add"],
        sudo: true,
    },
];

impl PackageManager {
    /// The argv this manager installs `package` with.
    pub fn install_argv(&self, package: &str) -> Vec<String> {
        let mut argv = Vec::new();
        if self.sudo {
            argv.push("sudo".to_owned());
        }
        argv.push(self.program.to_owned());
        argv.extend(self.argv.iter().map(|part| (*part).to_owned()));
        argv.push(package.to_owned());
        argv
    }

    /// The same, as the line shown before it runs.
    pub fn install_command(&self, package: &str) -> String {
        self.install_argv(package).join(" ")
    }
}

/// One command `setup` will run, in the order it will run them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    Register {
        path: String,
        label: String,
    },
    Adopt {
        path: String,
        label: String,
        name: String,
    },
    Settings {
        key: String,
        value: String,
    },
    ShellInit {
        file: String,
        shell: String,
    },
    TmuxInstall {
        command: String,
    },
    /// `body` is the exact text the step writes, decided when the plan is made
    /// so that what `--dry-run` prints and what `apply` writes cannot drift.
    TmuxConfig {
        file: String,
        created: bool,
        body: String,
    },
}

impl Step {
    /// The command line that produces this step, which is what `--dry-run`
    /// prints and what keeps `setup` honest about composing existing verbs.
    pub fn command(&self) -> String {
        match self {
            Self::Register { path, label } => {
                format!("wt register {} --label {label}", quote(path))
            }
            Self::Adopt { path, label, name } => {
                format!("wt adopt {} --label {label} --name {name}", quote(path))
            }
            // A settings write edits a structured document in place, so there
            // is no shell line that reproduces it. It is rendered as what it
            // does; every other step prints something runnable.
            Self::Settings { key, value } => {
                format!("# set {key} = {value} in $WT_HOME/config.toml")
            }
            // Byte-for-byte what `apply` appends, guard markers included: a
            // plan that prints a different line from the one it writes is not
            // a plan of the run.
            Self::ShellInit { file, shell } => format!(
                "cat >> {} <<'WT_SETUP'\n{}WT_SETUP",
                quote(file),
                shell_block(shell)
            ),
            Self::TmuxInstall { command } => command.clone(),
            // As with `ShellInit`: the text is the step, so print it rather
            // than a description of it. A created file is truncated and an
            // existing one appended to, which is the difference `apply` makes.
            Self::TmuxConfig {
                file,
                created,
                body,
            } => format!(
                "cat {} {} <<'WT_SETUP'\n{body}WT_SETUP",
                if *created { ">" } else { ">>" },
                quote(file)
            ),
        }
    }
}

fn quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Builds the ordered step list from the selected rows.
///
/// Registration precedes adoption for the same checkout because an adopt is
/// refused until its label exists (§11.6).
pub fn plan(buckets: &[Bucket], environment: Vec<Step>) -> Vec<Step> {
    let mut steps = environment;
    for bucket in buckets {
        for checkout in &bucket.checkouts {
            let label = checkout.label.clone();
            if checkout.selected && checkout.registered.is_none() {
                steps.push(Step::Register {
                    path: checkout.path.clone(),
                    label: label.clone(),
                });
            }
            let label_exists = checkout.registered.is_some()
                || (checkout.selected && checkout.registered.is_none());
            if !label_exists {
                continue;
            }
            for worktree in &checkout.worktrees {
                if worktree.selected && !worktree.registered {
                    steps.push(Step::Adopt {
                        path: worktree.path.clone(),
                        label: label.clone(),
                        name: worktree.name.clone(),
                    });
                }
            }
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn origin_spellings_share_one_key() {
        let expected = Some("github.com/acme/api".to_owned());
        assert_eq!(origin_key("git@github.com:acme/api.git"), expected);
        assert_eq!(origin_key("https://github.com/acme/api"), expected);
        assert_eq!(origin_key("https://github.com/acme/api.git"), expected);
        assert_eq!(origin_key("ssh://git@github.com/acme/api.git"), expected);
        assert_eq!(origin_key("ssh://git@github.com:22/acme/api.git"), expected);
        assert_eq!(origin_key("git@GitHub.com:Acme/API.git"), expected);
        assert_eq!(origin_key("https://github.com/acme/api/"), expected);
    }

    #[test]
    fn unusable_origins_have_no_key() {
        assert_eq!(origin_key(""), None);
        assert_eq!(origin_key("   "), None);
        assert_eq!(origin_key("github.com"), None);
    }

    #[test]
    fn a_local_path_remote_still_keys_by_its_tail() {
        assert_eq!(
            origin_key("/srv/git/acme/api.git"),
            Some("srv/git/acme/api".to_owned())
        );
    }

    #[test]
    fn labels_disambiguate_with_the_organisation_then_a_suffix() {
        let mut taken = set(&[]);
        let first = propose_label(Some("github.com/acme/api"), "/src/api", &taken);
        assert_eq!(first, "api");
        taken.insert(first);
        let second = propose_label(Some("github.com/contoso/api"), "/oss/api", &taken);
        assert_eq!(second, "contoso-api");
        taken.insert(second);
        let third = propose_label(Some("github.com/acme/api"), "/other/api", &taken);
        assert_eq!(third, "acme-api");
    }

    #[test]
    fn a_label_never_collides_with_a_registered_one() {
        let taken = set(&["api", "acme-api"]);
        let label = propose_label(Some("github.com/acme/api"), "/src/api", &taken);
        assert_eq!(label, "api-2");
    }

    #[test]
    fn an_originless_checkout_falls_back_to_its_directory() {
        let taken = set(&[]);
        assert_eq!(
            propose_label(None, "/src/scratch-pad", &taken),
            "scratch-pad"
        );
    }

    #[test]
    fn sanitising_produces_valid_identifiers() {
        assert_eq!(sanitise_label("My Repo!"), Some("my-repo".to_owned()));
        assert_eq!(sanitise_label("...."), None);
        assert_eq!(sanitise_label("--lead"), Some("lead".to_owned()));
        assert_eq!(
            sanitise_name("canonical"),
            Some("canonical-tree".to_owned())
        );
        assert_eq!(sanitise_label(&"x".repeat(50)), Some("x".repeat(32)));
    }

    #[test]
    fn a_worktree_name_comes_from_its_branch_tail() {
        let taken = set(&["canonical"]);
        assert_eq!(
            propose_name(Some("feature/fix-scrolling"), "/t/wt-1", &taken),
            "fix-scrolling"
        );
        let taken = set(&["canonical", "fix-scrolling"]);
        assert_eq!(
            propose_name(Some("feature/fix-scrolling"), "/t/wt-1", &taken),
            "feature-fix-scrolling"
        );
    }

    fn checkout(path: &str, origin: Option<&str>, touched: u64) -> CheckoutObs {
        CheckoutObs {
            path: path.to_owned(),
            common_gitdir: format!("{path}/.git"),
            origin: origin.map(str::to_owned),
            touched,
            worktrees: Vec::new(),
            registered: None,
            taken_names: Vec::new(),
        }
    }

    const NOW: u64 = 1_000 * SECONDS_PER_DAY;

    #[test]
    fn checkouts_of_one_origin_share_a_bucket_and_nothing_is_ticked_for_them() {
        let buckets = bucket(
            vec![
                checkout(
                    "/src/api-old",
                    Some("git@github.com:acme/api.git"),
                    NOW - 100,
                ),
                checkout("/src/api", Some("https://github.com/acme/api"), NOW - 10),
            ],
            &set(&[]),
        );
        assert_eq!(buckets.len(), 1, "one origin is one bucket");
        let rows = &buckets[0].checkouts;
        assert_eq!(rows[0].path, "/src/api", "most recent first");
        assert!(!rows[0].secondary);
        assert!(rows[1].secondary);
        assert_ne!(rows[0].label, rows[1].label);
        assert!(
            rows.iter().all(|row| !row.selected),
            "registering is opt in; recency only orders"
        );
    }

    #[test]
    fn a_stale_checkout_is_offered_but_not_selected() {
        let buckets = bucket(
            vec![checkout(
                "/src/old",
                Some("git@h:o/old.git"),
                NOW - 60 * SECONDS_PER_DAY,
            )],
            &set(&[]),
        );
        assert!(!buckets[0].checkouts[0].selected);
        assert!(buckets[0].stale(NOW, RECENT_DAYS));
    }

    #[test]
    fn a_registered_checkout_keeps_its_label_and_is_not_reoffered() {
        let mut observed = checkout("/src/api", Some("git@h:o/api.git"), NOW);
        observed.registered = Some("existing".to_owned());
        let buckets = bucket(vec![observed], &set(&["existing"]));
        let row = &buckets[0].checkouts[0];
        assert_eq!(row.label, "existing");
        assert!(!row.selected, "already registered is not a step");
    }

    #[test]
    fn worktrees_follow_their_checkout_and_sort_by_recency() {
        let mut observed = checkout("/src/api", Some("git@h:o/api.git"), NOW);
        observed.worktrees = vec![
            WorktreeObs {
                path: "/t/one".to_owned(),
                branch: Some("one".to_owned()),
                touched: NOW - 60 * SECONDS_PER_DAY,
                registered: false,
            },
            WorktreeObs {
                path: "/t/two".to_owned(),
                branch: Some("two".to_owned()),
                touched: NOW,
                registered: false,
            },
        ];
        let buckets = bucket(vec![observed], &set(&[]));
        let worktrees = &buckets[0].checkouts[0].worktrees;
        assert_eq!(worktrees[0].name, "two", "most recent first");
        assert!(
            worktrees.iter().all(|worktree| !worktree.selected),
            "adopting is opt in too"
        );
    }

    #[test]
    fn a_worktree_of_an_unregistered_checkout_is_never_selected() {
        let mut observed = checkout(
            "/src/api",
            Some("git@h:o/api.git"),
            NOW - 99 * SECONDS_PER_DAY,
        );
        observed.worktrees = vec![WorktreeObs {
            path: "/t/one".to_owned(),
            branch: Some("one".to_owned()),
            touched: NOW,
            registered: false,
        }];
        let buckets = bucket(vec![observed], &set(&[]));
        let checkout_row = &buckets[0].checkouts[0];
        assert!(!checkout_row.selected);
        assert!(!checkout_row.worktrees[0].selected);
    }

    #[test]
    fn a_worktree_name_avoids_the_names_its_label_already_holds() {
        let mut observed = checkout("/src/api", Some("git@h:o/api.git"), NOW);
        observed.registered = Some("api".to_owned());
        observed.taken_names = vec!["feature".to_owned()];
        observed.worktrees = vec![WorktreeObs {
            path: "/t/one".to_owned(),
            branch: Some("feature".to_owned()),
            touched: NOW,
            registered: false,
        }];
        let buckets = bucket(vec![observed], &set(&["api"]));
        let name = &buckets[0].checkouts[0].worktrees[0].name;
        assert_ne!(
            name, "feature",
            "adopting under a live target's name addresses that tree instead"
        );
    }

    #[test]
    fn a_maximum_length_label_still_gets_a_distinct_suffix() {
        let stem = "x".repeat(32);
        let taken = set(&[&stem]);
        let proposed = propose_label(None, &format!("/src/{stem}"), &taken);
        assert!(proposed.len() <= 32);
        assert!(
            !taken.contains(&proposed),
            "returned an already-taken label"
        );
        assert!(proposed.ends_with("-2"), "{proposed}");
    }

    #[test]
    fn a_local_origin_labels_from_the_directory_it_was_cloned_to() {
        let buckets = bucket(
            vec![checkout("/src/api", Some("/srv/git/api-origin.git"), NOW)],
            &set(&[]),
        );
        assert_eq!(buckets[0].checkouts[0].label, "api");
        assert!(buckets[0].has_origin, "it still groups by the origin");
    }

    #[test]
    fn a_maximum_length_name_still_gets_a_distinct_suffix() {
        let stem = "y".repeat(64);
        let taken = set(&["canonical", &stem]);
        let proposed = propose_name(Some(&stem), &format!("/t/{stem}"), &taken);
        assert!(proposed.len() <= 64, "{proposed}");
        assert!(!taken.contains(&proposed));
        assert!(proposed.ends_with("-2"), "{proposed}");
    }

    #[test]
    fn a_file_url_and_the_path_it_names_share_one_bucket() {
        assert_eq!(
            origin_key("file:///srv/git/acme/api.git"),
            origin_key("/srv/git/acme/api.git")
        );
    }

    #[test]
    fn a_guard_counts_however_it_is_written_unless_commented_out() {
        assert!(block_installed("eval \"$(wt shell-init zsh)\"\n"));
        assert!(block_installed("wt shell-init fish | source\n"));
        assert!(block_installed(
            "command -v wt >/dev/null && eval \"$(wt shell-init zsh)\"\n"
        ));
        assert!(!block_installed("# eval \"$(wt shell-init zsh)\"\n"));
        assert!(!block_installed("nothing here\n"));
    }

    #[test]
    fn originless_checkouts_do_not_merge() {
        let buckets = bucket(
            vec![
                checkout("/src/one", None, NOW),
                checkout("/src/two", None, NOW - 1),
            ],
            &set(&[]),
        );
        assert_eq!(buckets.len(), 2);
        assert!(buckets.iter().all(|bucket| !bucket.has_origin));
    }

    #[test]
    fn a_plan_registers_before_it_adopts() {
        let mut observed = checkout("/src/api", Some("git@h:o/api.git"), NOW);
        observed.worktrees = vec![WorktreeObs {
            path: "/t/one".to_owned(),
            branch: Some("one".to_owned()),
            touched: NOW,
            registered: false,
        }];
        let mut buckets = bucket(vec![observed], &set(&[]));
        buckets[0].checkouts[0].selected = true;
        buckets[0].checkouts[0].worktrees[0].selected = true;
        let steps = plan(&buckets, Vec::new());
        assert_eq!(
            steps,
            vec![
                Step::Register {
                    path: "/src/api".to_owned(),
                    label: "api".to_owned()
                },
                Step::Adopt {
                    path: "/t/one".to_owned(),
                    label: "api".to_owned(),
                    name: "one".to_owned()
                },
            ]
        );
        assert_eq!(steps[0].command(), "wt register /src/api --label api");
    }

    #[test]
    fn a_plan_adopts_into_an_already_registered_label() {
        let mut observed = checkout("/src/api", Some("git@h:o/api.git"), NOW);
        observed.registered = Some("api".to_owned());
        observed.worktrees = vec![WorktreeObs {
            path: "/t/one".to_owned(),
            branch: Some("one".to_owned()),
            touched: NOW,
            registered: false,
        }];
        let mut buckets = bucket(vec![observed], &set(&["api"]));
        buckets[0].checkouts[0].worktrees[0].selected = true;
        let steps = plan(&buckets, Vec::new());
        assert_eq!(
            steps,
            vec![Step::Adopt {
                path: "/t/one".to_owned(),
                label: "api".to_owned(),
                name: "one".to_owned()
            }]
        );
    }

    #[test]
    fn the_shell_step_prints_exactly_what_it_writes() {
        for shell in SHELLS {
            let step = Step::ShellInit {
                file: "/home/me/.zshrc".to_owned(),
                shell: (*shell).to_owned(),
            };
            let printed = step.command();
            let block = shell_block(shell);
            assert!(
                printed.contains(&block),
                "{shell}: the plan omits what apply writes\nplan: {printed}\nblock: {block}"
            );
            assert!(
                printed.contains(BLOCK_OPEN),
                "{shell}: the guard is missing"
            );
        }
        // fish is not POSIX; printing an `eval` line for it would install
        // something the shell cannot run.
        let fish = Step::ShellInit {
            file: "/c".to_owned(),
            shell: "fish".to_owned(),
        };
        assert!(fish.command().contains("| source"));
        assert!(!fish.command().contains("eval \"$("));
    }

    #[test]
    fn quoting_protects_a_path_with_a_space() {
        let step = Step::Register {
            path: "/src/my repo".to_owned(),
            label: "repo".to_owned(),
        };
        assert_eq!(step.command(), "wt register '/src/my repo' --label repo");
    }
}
