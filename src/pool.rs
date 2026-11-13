//! Pools of named slots shared by every tree on the machine, and leases on
//! them.
//!
//! A slot is leased while a process holds an exclusive `flock` on
//! `~/.wt/pools/<pool>/<n>.lock`. The kernel drops that lock when the
//! process exits, however it exits, so a crashed holder never strands a
//! slot. Next to each lock, `<n>.json` records who holds the slot and how it
//! was last used. Only the lock's holder writes the record, and its holder
//! field means something only while the lock is actually held: a process
//! that died leaves its holder behind, and the free lock shows it is stale.

use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_at};
use crate::{Tree, Wt, fsx, names, template};

const DEFAULT_WAIT: Duration = Duration::from_secs(30 * 60);

/// How often a lease waiting for a busy pool looks for a free slot.
const POLL: Duration = Duration::from_millis(100);

const RECORD_VERSION: u32 = 1;

/// A pool's definition as it applies to one lease: the invoking tree's
/// `[pool.<name>]`, with `size`, `wait` and `idle` overridden by the
/// machine's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Pool {
    pub name: String,
    pub size: u32,
    /// Template for slot names, where `{{n}}` is the slot number from 1.
    pub slot_name: String,
    pub wait: Duration,
    pub acquire: Option<String>,
    pub idle: Option<Duration>,
    pub retire: Option<String>,
}

impl Pool {
    /// The name of slot `n`, counting from 1.
    pub fn slot(&self, n: u32) -> Result<String> {
        let vars = BTreeMap::from([("n".to_owned(), n.to_string())]);
        template::render(&self.slot_name, &vars, &format!("pool.{}.name", self.name))
    }
}

/// The process holding a slot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    /// The tree that leased the slot, if it was leased from inside one.
    pub tree: Option<String>,
    /// Unix seconds.
    pub since: u64,
    /// Held to run `retire` rather than by a lease.
    #[serde(default)]
    pub retiring: bool,
}

/// One slot, as [`Wt::leases`] reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotInfo {
    pub pool: String,
    pub n: u32,
    pub name: String,
    pub leased: bool,
    /// Who holds the slot, when it is leased and the holder has recorded
    /// itself.
    pub holder: Option<Holder>,
    /// The tree that last released the slot; `None` if that lease was taken
    /// outside a tree.
    pub last_tree: Option<String>,
    /// Unix seconds.
    pub released_at: Option<u64>,
    /// Unix seconds from which an idle slot is due for `retire`, when it
    /// will be.
    pub retire_due: Option<u64>,
}

/// A slot [`Wt::reap_idle`] ran `retire` on.
#[derive(Debug)]
pub struct Retired {
    pub pool: String,
    pub slot: String,
    pub result: Result<()>,
}

/// Slots held for the life of this value. Dropping it releases them.
#[derive(Debug)]
pub struct Lease {
    tree: Option<String>,
    held: Vec<Held>,
}

#[derive(Debug)]
struct Held {
    pool: String,
    slot: String,
    record: PathBuf,
    /// Holding the open file is holding the lock.
    _lock: File,
}

impl Lease {
    /// `(pool, slot name)` for each pool leased, in the order they were
    /// taken. Pools skipped because this process already holds them are
    /// not included.
    pub fn slots(&self) -> impl Iterator<Item = (&str, &str)> {
        self.held
            .iter()
            .map(|held| (held.pool.as_str(), held.slot.as_str()))
    }

    /// `WT_LEASE_<POOL>` → slot name, for the command run under the lease.
    pub fn vars(&self) -> BTreeMap<String, String> {
        self.slots()
            .map(|(pool, slot)| (lease_var(pool), slot.to_owned()))
            .collect()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let now = unix_now();
        for held in self.held.drain(..).rev() {
            // The record is written before the lock is let go, so whoever
            // takes the slot next sees how it was left.
            if let Ok(mut record) = read_record(&held.record) {
                record.holder = None;
                record.released_at = Some(now);
                record.last_tree = self.tree.clone();
                let _ = write_record(&held.record, &record);
            }
        }
    }
}

/// What wt remembers about a slot between leases.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Record {
    version: u32,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    holder: Option<Holder>,
    last_tree: Option<String>,
    released_at: Option<u64>,
    /// Acquired since it was last retired.
    warm: bool,
    /// Retirement runs outside any tree and possibly long after the config
    /// that acquired the slot was last read, so the command, the checkout
    /// to run it in and the idle time are kept here.
    retire: Option<String>,
    retire_cwd: Option<PathBuf>,
    idle_secs: Option<u64>,
}

impl Record {
    /// Whether the slot has been idle long enough to retire. Release times
    /// are whole seconds, so the comparison is too, erring late rather than
    /// early.
    fn retire_due(&self, now: u64) -> bool {
        self.warm
            && self.retire.is_some()
            && self.retire_cwd.is_some()
            && matches!(
                (self.released_at, self.idle_secs),
                (Some(at), Some(idle)) if now.saturating_sub(at) > idle
            )
    }
}

impl Wt {
    /// The definition of `pool` for a lease from `tree`: its `[pool.<name>]`
    /// in the tree's config, if any, with `size`, `wait` and `idle` taken
    /// from the machine settings where they set them. A pool known only to
    /// the settings is a plain set of tokens, with no `acquire` or `retire`.
    pub fn pool(&self, tree: Option<&Tree>, pool: &str) -> Result<Pool> {
        let from_config = match tree {
            Some(tree) => tree.config()?.pools.remove(pool),
            None => None,
        };
        let sizing = self.settings.pools.get(pool);
        if from_config.is_none() && sizing.is_none() {
            let settings = self.home.join("config.toml");
            return Err(Error::NotFound(match tree {
                Some(tree) => format!(
                    "no pool `{pool}` in {}'s config or in {}",
                    tree.id(),
                    settings.display()
                ),
                None => format!(
                    "no pool `{pool}` in {}; pools in a repository's config are only known inside its trees",
                    settings.display()
                ),
            }));
        }
        let mut config = from_config.unwrap_or_default();
        if let Some(sizing) = sizing {
            config.size = sizing.size.or(config.size);
            config.wait = sizing.wait.or(config.wait);
            config.idle = sizing.idle.or(config.idle);
        }
        let size = config.size.ok_or_else(|| {
            Error::Config(format!(
                "pool `{pool}` has no `size`; set it in the repository's config or in {}",
                self.home.join("config.toml").display()
            ))
        })?;
        let definition = Pool {
            name: pool.to_owned(),
            size,
            slot_name: config.name.unwrap_or_else(|| format!("{pool}-{{{{n}}}}")),
            wait: config.wait.unwrap_or(DEFAULT_WAIT),
            acquire: config.acquire,
            idle: config.idle,
            retire: config.retire,
        };
        // A bad name template is reported before anything is leased.
        definition.slot(1)?;
        Ok(definition)
    }

    /// Leases one slot in each of `pools` and runs each pool's `acquire` on
    /// the slot it got. Waits up to each pool's `wait` for a free slot.
    ///
    /// Pools whose `WT_LEASE_<POOL>` this process inherited are skipped: the
    /// caller already holds them, as when a command run under a lease runs
    /// another that asks for the same pool.
    pub fn lease(&self, tree: Option<&Tree>, pools: &[&str]) -> Result<Lease> {
        let mut pools = pools.to_vec();
        // Every process takes pools in the same order, so two commands that
        // each want the other's pool can never wait on each other forever.
        pools.sort_unstable();
        if let Some(pair) = pools.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(Error::Conflict(format!(
                "pool `{}` is named twice",
                pair[0]
            )));
        }
        pools.retain(|pool| std::env::var_os(lease_var(pool)).is_none());
        let definitions = pools
            .iter()
            .map(|pool| self.pool(tree, pool))
            .collect::<Result<Vec<_>>>()?;
        let env = match tree {
            Some(tree) if definitions.iter().any(|pool| pool.acquire.is_some()) => {
                Some(tree.env()?)
            }
            _ => None,
        };

        let mut lease = Lease {
            tree: tree.map(Tree::id),
            held: Vec::new(),
        };
        for pool in &definitions {
            let dir = self.pool_dir(&pool.name);
            let (n, lock) = take(pool, &dir, lease.tree.as_deref())?;
            let slot = pool.slot(n)?;
            let record_path = record_path(&dir, n);
            let mut record = read_record(&record_path)?;
            record.name = slot.clone();
            record.holder = Some(Holder {
                pid: std::process::id(),
                tree: lease.tree.clone(),
                since: unix_now(),
                retiring: false,
            });
            // Recorded before `acquire` runs, so whatever it starts is
            // retired later even if it fails or this process dies midway.
            // A lease of a pool that has neither leaves what the last
            // acquiring config recorded alone.
            if pool.acquire.is_some() || pool.retire.is_some() {
                record.warm = true;
                record.retire = pool.retire.clone();
                record.retire_cwd = tree.map(|tree| tree.main.clone());
                record.idle_secs = pool.idle.map(ceil_secs);
            }
            write_record(&record_path, &record)?;
            lease.held.push(Held {
                pool: pool.name.clone(),
                slot: slot.clone(),
                record: record_path,
                _lock: lock,
            });

            let Some(acquire) = &pool.acquire else {
                continue;
            };
            let mut command = shell(acquire);
            if let (Some(tree), Some(env)) = (tree, &env) {
                env.apply(&mut command);
                command.current_dir(&tree.root);
            }
            command.envs(lease.vars()).env("WT_LEASE", &slot);
            // Its output goes to stderr so that it never mixes into the
            // output of the command run under the lease, which may be piped.
            let stderr = std::io::stderr()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|source| Error::Io {
                    context: "duplicating stderr".to_owned(),
                    source,
                })?;
            command.stdin(Stdio::null()).stdout(stderr);
            let status = command.status().map_err(|source| Error::Io {
                context: format!("running `acquire` for {slot}"),
                source,
            })?;
            if !status.success() {
                return Err(Error::Command(format!(
                    "pool `{}`: `acquire` failed for {slot} ({status})",
                    pool.name
                )));
            }
        }
        Ok(lease)
    }

    /// Runs `retire` on every warm slot that nobody holds and that has been
    /// idle longer than its `idle`. The slot stays locked while `retire`
    /// runs, so nobody can lease it halfway through. A failure is reported
    /// in its [`Retired`] and the slot is tried again next time.
    pub fn reap_idle(&self) -> Result<Vec<Retired>> {
        let mut retired = Vec::new();
        for (pool, n) in self.known_slots()? {
            let dir = self.pool_dir(&pool);
            let path = record_path(&dir, n);
            // Checked without the lock first so that a sweep over idle
            // pools doesn't contend with leases.
            if !read_record(&path)?.retire_due(unix_now()) {
                continue;
            }
            let Some(lock) = try_lock(&lock_path(&dir, n))? else {
                continue;
            };
            let mut record = read_record(&path)?;
            if !record.retire_due(unix_now()) {
                continue;
            }
            let slot = slot_name(&record, &pool, n);
            record.holder = Some(Holder {
                pid: std::process::id(),
                tree: None,
                since: unix_now(),
                retiring: true,
            });
            write_record(&path, &record)?;
            let result = run_retire(&record, &pool, &slot);
            record.holder = None;
            if result.is_ok() {
                record.warm = false;
            }
            write_record(&path, &record)?;
            drop(lock);
            retired.push(Retired { pool, slot, result });
        }
        Ok(retired)
    }

    /// Every slot that has ever been leased, by pool and number.
    pub fn leases(&self) -> Result<Vec<SlotInfo>> {
        let mut slots = Vec::new();
        for (pool, n) in self.known_slots()? {
            let dir = self.pool_dir(&pool);
            // Taking the lock for a moment is the only way to tell whether
            // it is held; with it, the record read next is also settled.
            let lock = try_lock(&lock_path(&dir, n))?;
            let record = read_record(&record_path(&dir, n))?;
            let leased = lock.is_none();
            drop(lock);
            let retire_due = match (record.released_at, record.idle_secs) {
                (Some(at), Some(idle)) if !leased && record.warm && record.retire.is_some() => {
                    Some(at + idle + 1)
                }
                _ => None,
            };
            slots.push(SlotInfo {
                name: slot_name(&record, &pool, n),
                pool,
                n,
                leased,
                holder: if leased { record.holder } else { None },
                last_tree: record.last_tree,
                released_at: record.released_at,
                retire_due,
            });
        }
        Ok(slots)
    }

    fn pool_dir(&self, pool: &str) -> PathBuf {
        self.home.join("pools").join(pool)
    }

    /// `(pool, n)` for every slot with a record, sorted.
    fn known_slots(&self) -> Result<Vec<(String, u32)>> {
        let pools = self.home.join("pools");
        let mut slots = Vec::new();
        let Ok(entries) = fs::read_dir(&pools) else {
            return Ok(slots);
        };
        for entry in entries {
            let entry = entry.map_err(io_at("reading", &pools))?;
            let pool = entry.file_name().to_string_lossy().into_owned();
            if !names::valid(&pool) || !entry.path().is_dir() {
                continue;
            }
            let dir = entry.path();
            for file in fs::read_dir(&dir).map_err(io_at("reading", &dir))? {
                let file = file.map_err(io_at("reading", &dir))?;
                let file_name = file.file_name();
                let n = file_name
                    .to_str()
                    .and_then(|name| name.strip_suffix(".json"))
                    .and_then(|n| n.parse::<u32>().ok());
                if let Some(n) = n {
                    slots.push((pool.clone(), n));
                }
            }
        }
        slots.sort();
        Ok(slots)
    }
}

/// Takes a free slot in `pool`, preferring the one `tree` used last, then
/// one still warm from an earlier `acquire`, then the lowest-numbered.
/// Polls until the pool's `wait` runs out.
fn take(pool: &Pool, dir: &Path, tree: Option<&str>) -> Result<(u32, File)> {
    let started = Instant::now();
    loop {
        let mut candidates = Vec::new();
        for n in 1..=pool.size {
            let record = read_record(&record_path(dir, n))?;
            let theirs = tree.is_some() && record.last_tree.as_deref() == tree;
            candidates.push((!theirs, !record.warm, n));
        }
        candidates.sort_unstable();
        for (_, _, n) in candidates {
            if let Some(lock) = try_lock(&lock_path(dir, n))? {
                return Ok((n, lock));
            }
        }
        let waited = started.elapsed();
        if waited >= pool.wait {
            return Err(busy(pool, dir));
        }
        std::thread::sleep(POLL.min(pool.wait - waited));
    }
}

/// The error for a pool that stayed full, saying who holds each slot.
fn busy(pool: &Pool, dir: &Path) -> Error {
    let now = unix_now();
    let holders: Vec<String> = (1..=pool.size)
        .map(|n| {
            let record = read_record(&record_path(dir, n)).unwrap_or_default();
            let slot = pool.slot(n).unwrap_or_else(|_| n.to_string());
            match record.holder {
                Some(holder) => {
                    let what = match (&holder.tree, holder.retiring) {
                        (_, true) => "being retired".to_owned(),
                        (Some(tree), false) => format!("leased by {tree}"),
                        (None, false) => "leased outside a tree".to_owned(),
                    };
                    format!(
                        "{slot} {what} (pid {}, for {})",
                        holder.pid,
                        span(now.saturating_sub(holder.since))
                    )
                }
                None => format!("{slot} is held"),
            }
        })
        .collect();
    Error::Busy(format!(
        "no free slot in pool `{}` after waiting {}: {}",
        pool.name,
        if pool.wait < Duration::from_secs(1) {
            format!("{}ms", pool.wait.as_millis())
        } else {
            span(pool.wait.as_secs())
        },
        holders.join("; ")
    ))
}

fn run_retire(record: &Record, pool: &str, slot: &str) -> Result<()> {
    let (Some(retire), Some(cwd)) = (&record.retire, &record.retire_cwd) else {
        return Ok(());
    };
    if !cwd.is_dir() {
        return Err(Error::Command(format!(
            "can't run `retire` for {slot}: {} no longer exists",
            cwd.display()
        )));
    }
    let output = shell(retire)
        .current_dir(cwd)
        .env("WT_LEASE", slot)
        .env(lease_var(pool), slot)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| Error::Io {
            context: format!("running `retire` for {slot}"),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
    let mut message = format!("`retire` failed for {slot} ({})", output.status);
    for line in tail.into_iter().rev() {
        message.push_str("\n  ");
        message.push_str(line);
    }
    Err(Error::Command(message))
}

/// `WT_LEASE_<POOL>`.
fn lease_var(pool: &str) -> String {
    format!("WT_LEASE_{}", names::env_suffix(pool))
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script);
    command
}

fn lock_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{n}.lock"))
}

fn record_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{n}.json"))
}

fn slot_name(record: &Record, pool: &str, n: u32) -> String {
    if record.name.is_empty() {
        format!("{pool}-{n}")
    } else {
        record.name.clone()
    }
}

/// Opens and locks `path` without waiting; `None` when another open file
/// holds the lock.
fn try_lock(path: &Path) -> Result<Option<File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_at("creating", parent))?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io_at("opening", path))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(io_at("locking", path)(error)),
    }
}

fn read_record(path: &Path) -> Result<Record> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Record::default());
        }
        Err(error) => return Err(io_at("reading", path)(error)),
    };
    let record: Record = serde_json::from_str(&text)
        .map_err(|error| Error::State(format!("{} is not valid: {error}", path.display())))?;
    if record.version > RECORD_VERSION {
        return Err(Error::State(format!(
            "{} was written by a newer wt (version {}); upgrade wt",
            path.display(),
            record.version
        )));
    }
    Ok(record)
}

fn write_record(path: &Path, record: &Record) -> Result<()> {
    let record = Record {
        version: RECORD_VERSION,
        ..record.clone()
    };
    let mut text = serde_json::to_string_pretty(&record).expect("slot records serialise");
    text.push('\n');
    fsx::write_atomic(path, text.as_bytes(), None)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

fn ceil_secs(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_nanos() > 0)
}

/// `45s`, `12m`, `3h`.
fn span(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_retirement_is_never_early() {
        let record = Record {
            warm: true,
            retire: Some("true".into()),
            retire_cwd: Some("/".into()),
            released_at: Some(100),
            idle_secs: Some(1),
            ..Record::default()
        };
        assert!(!record.retire_due(101));
        assert!(record.retire_due(102));
        let cold = Record {
            warm: false,
            ..record.clone()
        };
        assert!(!cold.retire_due(1000));
    }

    #[test]
    fn records_from_a_newer_wt_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1.json");
        fs::write(&path, r#"{"version": 9}"#).unwrap();
        let error = read_record(&path).unwrap_err().to_string();
        assert!(error.contains("upgrade wt"), "{error}");
    }
}
