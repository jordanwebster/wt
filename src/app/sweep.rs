//! Reclaiming build output the workspace will never read again (SPEC §11.9).
//!
//! The decision is `wt_core::sweep::plan`; this module observes one cargo
//! build directory into the snapshot that decision consumes, asks cargo
//! which crate roots the workspace resolves to, and applies the plan under
//! cargo's own build-directory lock.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wt_core::config::{Output, OutputKind as Root};
use wt_core::report::{Notice, NoticeLevel, SweepReport};
use wt_core::sweep::{FileObs, IncrementalObs, OutputKind, Plan, Snapshot, UnitObs};
use wt_core::CoreError;

use super::Context;

/// One build directory's pass: what it would or did reclaim, or why it
/// stood down.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Pass {
    pub build_dir: String,
    pub report: SweepReport,
    pub skipped: Option<String>,
}

impl Pass {
    pub fn reclaims(&self) -> bool {
        self.skipped.is_none()
            && (self.report.units > 0 || self.report.incremental > 0 || self.report.files > 0)
    }
}

/// Runs every sweep the tree's adapters declare. With `apply` false the
/// passes only say what they would delete (prune's plan); with it true they
/// delete under the build-directory lock.
pub(crate) fn sweep_tree(
    context: &Context,
    tree: &wt_core::model::TreeRec,
    apply: bool,
) -> Result<Vec<Pass>, CoreError> {
    let config = context.load_config(tree)?;
    let root = Path::new(tree.path.as_str());
    let mut passes = Vec::new();
    for output in &config.outputs {
        let build_dir = root.join(output.path.as_str());
        if !matches!(
            wt_sys::fsx::path_kind(&build_dir)?,
            wt_sys::fsx::PathKind::Directory
        ) {
            continue;
        }
        let pass = match output.kind {
            Root::Opaque => continue,
            Root::Cargo => cargo_pass(context, root, output, &build_dir, apply),
        };
        passes.push(match pass {
            Ok(pass) => pass,
            Err(error) => Pass {
                build_dir: output.path.to_string(),
                report: SweepReport::default(),
                skipped: Some(error.message),
            },
        });
    }
    // The next sweep compares against what the directories hold now, so a
    // pass that stood down is retried and one that ran is
    // not repeated for nothing.
    if apply && passes.iter().all(|pass| pass.skipped.is_none()) {
        store_sample(context, tree)?;
    }
    Ok(passes)
}

/// What the tree's build directories hold, as cheaply as it can be told
/// apart from what they held at the last sweep: for each profile directory,
/// the fingerprint directory's own modification time, its entry count, and
/// its newest entry. Cargo creates a unit's fingerprint directory when it
/// first compiles the unit and deletes and rewrites the fingerprint file on
/// every recompile, so anything that produced output — a hook, or a
/// build run by hand in the tree — moves the sample, and a command that
/// produced nothing leaves it alone.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Sample(Vec<(String, u64, usize, u64)>);

pub(crate) fn sample(
    context: &Context,
    tree: &wt_core::model::TreeRec,
) -> Result<Sample, CoreError> {
    let config = context.load_config(tree)?;
    let root = Path::new(tree.path.as_str());
    let mut sample = Vec::new();
    for output in &config.outputs {
        if output.kind == Root::Opaque {
            continue;
        }
        let build_dir = root.join(output.path.as_str());
        if !matches!(
            wt_sys::fsx::path_kind(&build_dir)?,
            wt_sys::fsx::PathKind::Directory
        ) {
            continue;
        }
        for profile in profile_dirs(&build_dir)? {
            let fingerprints = profile.join(".fingerprint");
            let own = wt_sys::fsx::modified_secs(&fingerprints)?.unwrap_or(0);
            let entries = wt_sys::fsx::read_dir_paths(&fingerprints)?;
            let newest = entries
                .iter()
                .map(|entry| wt_sys::fsx::modified_secs(entry).map(|at| at.unwrap_or(0)))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .max()
                .unwrap_or(0);
            sample.push((
                profile.to_string_lossy().into_owned(),
                own,
                entries.len(),
                newest,
            ));
        }
    }
    Ok(Sample(sample))
}

/// Where the sample taken after the last sweep of a tree is kept.
fn sample_path(tree: &wt_core::model::TreeRec) -> PathBuf {
    Path::new(tree.path.as_str()).join(".wt/sweep.sample")
}

fn store_sample(context: &Context, tree: &wt_core::model::TreeRec) -> Result<(), CoreError> {
    let sample = sample(context, tree)?;
    let text = serde_json::to_string(&sample).expect("a sample serializes");
    wt_sys::fsx::write_store(&sample_path(tree), text.as_bytes())
}

pub(crate) fn notices(target: &str, passes: &[Pass]) -> Vec<Notice> {
    passes
        .iter()
        .filter_map(|pass| {
            if let Some(reason) = &pass.skipped {
                Some(Notice {
                    level: NoticeLevel::Info,
                    code: "SWEEP_SKIPPED".to_owned(),
                    subject: Some(target.to_owned()),
                    message: format!("{} was not swept: {reason}", pass.build_dir),
                })
            } else if pass.reclaims() {
                Some(Notice {
                    level: NoticeLevel::Info,
                    code: "SWEPT".to_owned(),
                    subject: Some(target.to_owned()),
                    message: format!(
                        "removed {} superseded unit{}, {} stale incremental director{} and {} stale file{} from {} ({} MB)",
                        pass.report.units,
                        if pass.report.units == 1 { "" } else { "s" },
                        pass.report.incremental,
                        if pass.report.incremental == 1 { "y" } else { "ies" },
                        pass.report.files,
                        if pass.report.files == 1 { "" } else { "s" },
                        pass.build_dir,
                        pass.report.kb / 1024
                    ),
                })
            } else {
                None
            }
        })
        .collect()
}

pub(crate) fn total(passes: &[Pass]) -> SweepReport {
    passes
        .iter()
        .fold(SweepReport::default(), |mut total, pass| {
            total.units += pass.report.units;
            total.incremental += pass.report.incremental;
            total.files += pass.report.files;
            total.kb += pass.report.kb;
            total
        })
}

fn cargo_pass(
    context: &Context,
    root: &Path,
    output: &Output,
    build_dir: &Path,
    apply: bool,
) -> Result<Pass, CoreError> {
    let workspace = if output.workspace == "." {
        root.to_path_buf()
    } else {
        root.join(&output.workspace)
    };
    let resolve = cargo_metadata(context, &workspace)?;
    let now = wt_sys::fsx::epoch_seconds();
    let mut pass = Pass {
        build_dir: output.path.to_string(),
        ..Pass::default()
    };
    for profile in profile_dirs(build_dir)? {
        // Cargo's own build-directory lock: a build in flight holds it, and a
        // sweep in flight keeps a build out, exactly as two builds exclude
        // each other. Held means a build is running; the next one sweeps.
        let held = if apply {
            match wt_sys::fsx::try_lock_exclusive(&profile.join(".cargo-lock"))? {
                Some(lock) => Some(lock),
                None => {
                    pass.skipped = Some(format!(
                        "a build holds {}",
                        profile.join(".cargo-lock").display()
                    ));
                    return Ok(pass);
                }
            }
        } else {
            None
        };
        let layout = Layout::observe(&profile)?;
        let snapshot = Snapshot {
            units: layout.units(&resolve.workspace_root)?,
            workspace_roots: resolve.workspace_roots.clone(),
            files: layout.files()?,
            incremental: layout.incremental()?,
            now,
        };
        let plan = wt_core::sweep::plan(&snapshot);
        if let Some(reason) = plan.refused {
            pass.skipped = Some(format!("{}: {reason}", profile.display()));
            return Ok(pass);
        }
        let report = layout.measure(&plan)?;
        if apply {
            layout.delete(&plan)?;
        }
        pass.report.units += report.units;
        pass.report.incremental += report.incremental;
        pass.report.files += report.files;
        pass.report.kb += report.kb;
        drop(held);
    }
    Ok(pass)
}

struct Resolve {
    workspace_root: PathBuf,
    workspace_roots: BTreeSet<String>,
}

/// Asks cargo for the current resolve without touching the network or the
/// lockfile: every target source path of every workspace member — a package
/// without a `source`, which is what path packages are — and the workspace
/// root relative dep-info paths resolve against.
fn cargo_metadata(context: &Context, workspace: &Path) -> Result<Resolve, CoreError> {
    let mut request = wt_sys::proc::CommandRequest::new("cargo");
    request.args =
        wt_sys::proc::os_args(&["metadata", "--format-version", "1", "--locked", "--offline"]);
    request.cwd = Some(workspace.to_path_buf());
    request
        .env
        .insert("CARGO_TERM_COLOR".to_owned(), "never".to_owned());
    let timeout = super::context::duration(
        context.settings.git.timeouts.query.as_deref(),
        Duration::from_secs(30),
    )
    .max(Duration::from_secs(30));
    let output = wt_sys::proc::capture_op(&request, timeout, Some("cargo metadata"))?;
    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("cargo metadata failed")
            .to_owned();
        return Err(CoreError::new(
            wt_core::ExitClass::External,
            "SWEEP_SKIPPED",
            format!("cargo metadata did not answer: {reason}"),
            "run `cargo metadata --locked --offline` in the tree to see why",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        CoreError::new(
            wt_core::ExitClass::External,
            "SWEEP_SKIPPED",
            format!("cargo metadata is not JSON: {error}"),
            "run `cargo metadata --locked --offline` in the tree to see why",
        )
    })?;
    let workspace_root = value["workspace_root"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.to_path_buf());
    let workspace_roots = value["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|package| package["source"].is_null())
        .flat_map(|package| package["targets"].as_array().into_iter().flatten())
        .filter_map(|target| target["src_path"].as_str())
        .map(str::to_owned)
        .collect();
    Ok(Resolve {
        workspace_root,
        workspace_roots,
    })
}

/// Every profile directory under a cargo build directory: `debug`,
/// `release`, and `<triple>/<profile>` for cross builds — whatever holds a
/// `.fingerprint`.
fn profile_dirs(build_dir: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let mut profiles = Vec::new();
    for entry in wt_sys::fsx::read_dir_paths(build_dir)? {
        if !matches!(
            wt_sys::fsx::path_kind(&entry)?,
            wt_sys::fsx::PathKind::Directory
        ) {
            continue;
        }
        if is_profile(&entry)? {
            profiles.push(entry);
            continue;
        }
        for nested in wt_sys::fsx::read_dir_paths(&entry)? {
            if matches!(
                wt_sys::fsx::path_kind(&nested)?,
                wt_sys::fsx::PathKind::Directory
            ) && is_profile(&nested)?
            {
                profiles.push(nested);
            }
        }
    }
    Ok(profiles)
}

fn is_profile(dir: &Path) -> Result<bool, CoreError> {
    Ok(matches!(
        wt_sys::fsx::path_kind(&dir.join(".fingerprint"))?,
        wt_sys::fsx::PathKind::Directory
    ))
}

/// One profile directory's files, indexed by unit hash.
struct Layout {
    profile: PathBuf,
    /// Fingerprint directories: hash → path.
    fingerprints: BTreeMap<String, PathBuf>,
    /// `deps/` entries carrying a hash: hash → paths.
    deps: BTreeMap<String, Vec<PathBuf>>,
    /// `build/<package>-<hash>` directories: hash → path.
    builds: BTreeMap<String, PathBuf>,
    /// Every hash-carrying entry under `deps/` and `build/`, as the decision
    /// sees it.
    files: Vec<FileObs>,
}

impl Layout {
    fn observe(profile: &Path) -> Result<Self, CoreError> {
        let mut fingerprints = BTreeMap::new();
        for path in wt_sys::fsx::read_dir_paths(&profile.join(".fingerprint"))? {
            if let Some(hash) = unit_hash(&path.file_name().unwrap_or_default().to_string_lossy()) {
                fingerprints.insert(hash, path);
            }
        }
        let mut deps = BTreeMap::<String, Vec<PathBuf>>::new();
        let mut files = Vec::new();
        for path in wt_sys::fsx::read_dir_paths(&profile.join("deps"))? {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let Some(hash) = unit_hash(&name) else {
                continue;
            };
            let Some(stat) = wt_sys::fsx::link_stat(&path)? else {
                continue;
            };
            files.push(FileObs {
                path: format!("deps/{name}"),
                hash: hash.clone(),
                object: !stat.is_dir && path.extension().is_some_and(|ext| ext == "o"),
                links: stat.links,
                modified_at: stat.modified_secs,
            });
            deps.entry(hash).or_default().push(path.clone());
        }
        let mut builds = BTreeMap::new();
        for path in wt_sys::fsx::read_dir_paths(&profile.join("build"))? {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let Some(hash) = unit_hash(&name) else {
                continue;
            };
            let Some(stat) = wt_sys::fsx::link_stat(&path)? else {
                continue;
            };
            files.push(FileObs {
                path: format!("build/{name}"),
                hash: hash.clone(),
                object: false,
                links: stat.links,
                modified_at: stat.modified_secs,
            });
            builds.insert(hash, path);
        }
        Ok(Self {
            profile: profile.to_path_buf(),
            fingerprints,
            deps,
            builds,
            files,
        })
    }

    fn files(&self) -> Result<Vec<FileObs>, CoreError> {
        Ok(self.files.clone())
    }

    fn units(&self, workspace_root: &Path) -> Result<Vec<UnitObs>, CoreError> {
        let mut units = Vec::new();
        for (hash, dir) in &self.fingerprints {
            let Some(kind) = fingerprint_kind(dir)? else {
                continue;
            };
            let json: serde_json::Value =
                wt_sys::fsx::read_string(&dir.join(format!("{kind}.json")))?
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or(serde_json::Value::Null);
            let fingerprint = wt_sys::fsx::read_string(&dir.join(&kind))?
                .and_then(|text| parse_le_hex(text.trim()));
            let deps = json["deps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|dep| dep.get(3).and_then(serde_json::Value::as_u64))
                .collect();
            let identity = [
                "features",
                "target",
                "profile",
                "path",
                "compile_kind",
                "rustflags",
                "config",
            ]
            .iter()
            .map(|field| format!("{field}={}", json[*field]))
            .collect::<Vec<_>>()
            .join(";");
            let crate_root = if kind.starts_with("run-") {
                None
            } else if kind.starts_with("build-script-") {
                self.builds
                    .get(hash)
                    .map(|build| dep_info_in(build, workspace_root))
                    .transpose()?
                    .flatten()
            } else {
                self.deps
                    .get(hash)
                    .into_iter()
                    .flatten()
                    .find(|path| path.extension().is_some_and(|ext| ext == "d"))
                    .map(|path| crate_root_of(path, workspace_root))
                    .transpose()?
                    .flatten()
            };
            let compiled_at = wt_sys::fsx::modified_secs(&dir.join("invoked.timestamp"))?
                .or(wt_sys::fsx::modified_secs(dir)?)
                .unwrap_or(0);
            units.push(UnitObs {
                hash: hash.clone(),
                kind,
                fingerprint,
                deps,
                crate_root,
                identity,
                output: self.output_kind(hash)?,
                compiled_at,
            });
        }
        Ok(units)
    }

    fn output_kind(&self, hash: &str) -> Result<OutputKind, CoreError> {
        let mut kind = OutputKind::None;
        for path in self.deps.get(hash).into_iter().flatten() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let candidate = match path.extension().and_then(|ext| ext.to_str()) {
                Some("rlib" | "dylib" | "so" | "dll" | "a" | "wasm") => OutputKind::Library,
                Some("rmeta") => OutputKind::Metadata,
                Some("d" | "dSYM" | "pdb" | "o") => OutputKind::None,
                Some("exe") => OutputKind::Executable,
                _ if !name.starts_with("lib") && wt_sys::fsx::is_executable_file(path)? => {
                    OutputKind::Executable
                }
                _ => OutputKind::None,
            };
            kind = kind.max(candidate);
        }
        if kind == OutputKind::None {
            if let Some(build) = self.builds.get(hash) {
                for path in wt_sys::fsx::read_dir_paths(build)? {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if name.starts_with("build_script_")
                        && path.extension().is_none()
                        && wt_sys::fsx::is_executable_file(&path)?
                    {
                        kind = OutputKind::Executable;
                    }
                }
            }
        }
        Ok(kind)
    }

    fn incremental(&self) -> Result<Vec<IncrementalObs>, CoreError> {
        let mut dirs = Vec::new();
        for path in wt_sys::fsx::read_dir_paths(&self.profile.join("incremental"))? {
            if !matches!(
                wt_sys::fsx::path_kind(&path)?,
                wt_sys::fsx::PathKind::Directory
            ) {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            // rustc names a session directory `<crate>-<hash>`, the hash
            // in its own base-36 spelling; the crate is what comes before
            // the last dash.
            let crate_name = name
                .rsplit_once('-')
                .map_or(name.as_str(), |(crate_name, _)| crate_name)
                .to_owned();
            dirs.push(IncrementalObs {
                name,
                crate_name,
                modified_at: wt_sys::fsx::modified_secs(&path)?.unwrap_or(0),
            });
        }
        Ok(dirs)
    }

    fn paths_of(&self, plan: &Plan) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for hash in &plan.dead_units {
            paths.extend(self.fingerprints.get(hash).cloned());
            paths.extend(self.builds.get(hash).cloned());
            paths.extend(self.deps.get(hash).into_iter().flatten().cloned());
        }
        for name in &plan.dead_incremental {
            paths.push(self.profile.join("incremental").join(name));
        }
        for file in &plan.dead_files {
            paths.push(self.profile.join(file));
        }
        paths
    }

    fn measure(&self, plan: &Plan) -> Result<SweepReport, CoreError> {
        let mut kb = 0;
        for path in self.paths_of(plan) {
            if !matches!(
                wt_sys::fsx::path_kind(&path)?,
                wt_sys::fsx::PathKind::Missing
            ) {
                kb += wt_sys::fsx::disk_kb(&path)?;
            }
        }
        Ok(SweepReport {
            units: plan.dead_units.len() as u64,
            incremental: plan.dead_incremental.len() as u64,
            files: plan.dead_files.len() as u64,
            kb,
        })
    }

    fn delete(&self, plan: &Plan) -> Result<(), CoreError> {
        for path in self.paths_of(plan) {
            wt_sys::fsx::remove_path(&path)?;
        }
        Ok(())
    }
}

/// The unit hash in a `deps/` or `build/` entry name: the first
/// `-<sixteen hex digits>` that ends the name or is followed by `.`.
/// `libserde-<hash>.rlib`, `wt-<hash>`, `wt-<hash>.dSYM` and a codegen
/// unit's `wt-<hash>.<cgu>.rcgu.o` all name their unit this way; the
/// hashes inside a codegen-unit name are never preceded by a dash, so they
/// do not qualify.
fn unit_hash(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut from = 0;
    while let Some(offset) = name[from..].find('-') {
        let start = from + offset + 1;
        let end = start + 16;
        if end <= bytes.len()
            && bytes[start..end].iter().all(u8::is_ascii_hexdigit)
            && (end == bytes.len() || bytes[end] == b'.')
        {
            return Some(name[start..end].to_owned());
        }
        from = start;
    }
    None
}

/// The fingerprint file's name inside a unit directory: the one with a
/// `.json` twin. `None` for a directory cargo left without one.
fn fingerprint_kind(dir: &Path) -> Result<Option<String>, CoreError> {
    for path in wt_sys::fsx::read_dir_paths(dir)? {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(kind) = name.strip_suffix(".json") {
            return Ok(Some(kind.to_owned()));
        }
    }
    Ok(None)
}

/// Cargo writes a fingerprint hash as the hex of its little-endian bytes.
fn parse_le_hex(text: &str) -> Option<u64> {
    if text.len() != 16 {
        return None;
    }
    let mut bytes = [0_u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(u64::from_le_bytes(bytes))
}

/// The crate root named by the first `.d` file in a build-script directory.
fn dep_info_in(build: &Path, workspace_root: &Path) -> Result<Option<String>, CoreError> {
    for path in wt_sys::fsx::read_dir_paths(build)? {
        if path.extension().is_some_and(|ext| ext == "d") {
            return crate_root_of(&path, workspace_root);
        }
    }
    Ok(None)
}

/// The crate root a rustc dep-info file records: the first prerequisite of
/// its first rule, made absolute against the workspace root cargo ran in.
fn crate_root_of(dep_info: &Path, workspace_root: &Path) -> Result<Option<String>, CoreError> {
    let Some(text) = wt_sys::fsx::read_string(dep_info)? else {
        return Ok(None);
    };
    Ok(text.lines().find_map(|line| {
        let (_, prerequisites) = line.split_once(": ")?;
        let first = first_make_token(prerequisites)?;
        let path = Path::new(&first);
        Some(if path.is_absolute() {
            first
        } else {
            workspace_root.join(path).to_string_lossy().into_owned()
        })
    }))
}

/// The first whitespace-delimited token of a Makefile prerequisite list,
/// with `\ ` unescaped.
fn first_make_token(text: &str) -> Option<String> {
    let mut token = String::new();
    let mut chars = text.trim_start().chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' if chars.peek() == Some(&' ') => {
                token.push(' ');
                chars.next();
            }
            ' ' | '\t' => break,
            other => token.push(other),
        }
    }
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_hashes_are_the_sixteen_hex_digits_after_a_dash() {
        assert_eq!(
            unit_hash("wt-core-7f01163ae9f9618b"),
            Some("7f01163ae9f9618b".to_owned())
        );
        assert_eq!(unit_hash("build-script-build"), None);
        assert_eq!(
            unit_hash("libserde-c8872ac183e5e9ac.rlib"),
            Some("c8872ac183e5e9ac".to_owned())
        );
        assert_eq!(
            unit_hash("wt-0123456789abcdef.dSYM"),
            Some("0123456789abcdef".to_owned())
        );
        assert_eq!(
            unit_hash("wt-0123456789abcdef"),
            Some("0123456789abcdef".to_owned())
        );
        // A codegen unit's object: the unit hash, not the codegen-unit hash.
        assert_eq!(
            unit_hash("fixture-391077e733a30568.00gl9pq6dplm4nifnwqr6oca9.09885ma.rcgu.o"),
            Some("391077e733a30568".to_owned())
        );
        assert_eq!(
            unit_hash("wt_core-0123456789abcdef.wt_core.fedcba9876543210-cgu.0.rcgu.o"),
            Some("0123456789abcdef".to_owned())
        );
        assert_eq!(unit_hash("libfoo-0123456789abcdefg.rlib"), None);
    }

    #[test]
    fn fingerprint_hashes_are_little_endian_hex() {
        assert_eq!(
            parse_le_hex("d403a2f9f15e7923"),
            Some(2_556_178_656_877_741_012)
        );
        assert_eq!(parse_le_hex("zz"), None);
    }

    #[test]
    fn dep_info_first_prerequisite_is_the_crate_root() {
        assert_eq!(
            first_make_token("crates/wt-core/src/lib.rs crates/wt-core/src/adapters.rs"),
            Some("crates/wt-core/src/lib.rs".to_owned())
        );
        assert_eq!(
            first_make_token("/a/dir\\ with\\ space/lib.rs /b.rs"),
            Some("/a/dir with space/lib.rs".to_owned())
        );
        assert_eq!(first_make_token("   "), None);
    }
}
