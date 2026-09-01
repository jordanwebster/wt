//! Where a command's wall time went, as one JSON object per event.
//!
//! This log measures and nothing else. wt already explains itself through
//! notices and error codes; the one thing those cannot say is how long
//! anything took, so every record here carries a duration and just enough
//! identity to group by — which invocation wrote it, and what was being done.
//!
//! Each record is a single `O_APPEND` write of at most [`MAX_RECORD`] bytes,
//! which POSIX makes atomic on a regular file. The doors wt runs in parallel
//! therefore append to one file without interleaving a line and without any of
//! them taking a lock. A record that would exceed the limit is truncated
//! rather than split.
//!
//! Recipe text never appears here. Argument text is recorded only where wt
//! composed it — its own `git` and `tmux` invocations — because a task recipe
//! is the user's, and can hold a credential.
//!
//! Nothing in this module reports failure to its caller. A command that did
//! its work has succeeded whether or not the measurement of it survived.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// POSIX guarantees an atomic append only up to `PIPE_BUF`.
const MAX_RECORD: usize = 4096;
/// Rotation keeps one previous file, checked once per invocation.
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

struct Sink {
    path: PathBuf,
    run: String,
    command: String,
    seq: AtomicU64,
    started: Instant,
}

static SINK: OnceLock<Option<Sink>> = OnceLock::new();

fn sink() -> Option<&'static Sink> {
    SINK.get().and_then(Option::as_ref)
}

/// Begins recording this invocation. Called once, before the work starts.
pub fn open(home: &Path, command: &str, enabled: bool) {
    let _ = SINK.set(enabled.then(|| {
        let path = home.join("logs/wt.jsonl");
        rotate(&path);
        Sink {
            path,
            run: run_id(),
            command: command.to_owned(),
            seq: AtomicU64::new(0),
            started: Instant::now(),
        }
    }));
}

/// Records one child process. The returned value measures until it is finished.
#[must_use]
pub fn spawn(program: &OsStr, op: Option<&str>) -> Child {
    Child {
        name: Path::new(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy()
            .into_owned(),
        op: op.map(str::to_owned),
        started: Instant::now(),
    }
}

pub struct Child {
    name: String,
    op: Option<String>,
    started: Instant,
}

impl Child {
    /// Records the child's duration and how it ended.
    pub fn finish(self, outcome: Outcome) {
        let mut record = serde_json::Map::new();
        record.insert("name".to_owned(), self.name.into());
        if let Some(op) = self.op {
            record.insert("op".to_owned(), op.into());
        }
        match outcome {
            Outcome::Code(code) => {
                record.insert("code".to_owned(), code.into());
            }
            Outcome::TimedOut => {
                record.insert("outcome".to_owned(), "timeout".into());
            }
            Outcome::Failed => {
                record.insert("outcome".to_owned(), "failed".into());
            }
            Outcome::Detached => {
                record.insert("outcome".to_owned(), "detached".into());
            }
        }
        emit("child", millis(self.started), record);
    }
}

pub enum Outcome {
    Code(i32),
    TimedOut,
    /// The child could not be spawned or could not be waited for.
    Failed,
    /// Started and deliberately left running.
    Detached,
}

/// Measures a stretch of work that is not a single child process.
#[must_use]
pub fn span(kind: &'static str, name: impl Into<String>) -> Span {
    Span {
        kind,
        name: name.into(),
        subject: None,
        started: Instant::now(),
    }
}

pub struct Span {
    kind: &'static str,
    name: String,
    subject: Option<String>,
    started: Instant,
}

impl Span {
    #[must_use]
    pub fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn finish(self) {
        if sink().is_none() {
            return;
        }
        let mut record = serde_json::Map::new();
        record.insert("name".to_owned(), self.name.into());
        if let Some(subject) = self.subject {
            record.insert("subject".to_owned(), subject.into());
        }
        emit(self.kind, millis(self.started), record);
    }
}

/// Closes the invocation with its total duration and exit code.
pub fn command_done(exit: i32) {
    let Some(sink) = sink() else { return };
    let mut record = serde_json::Map::new();
    record.insert("name".to_owned(), sink.command.clone().into());
    record.insert("code".to_owned(), exit.into());
    emit("cmd", millis(sink.started), record);
}

/// Closes the invocation at the point wt hands the process to another program.
/// The commands that exec never return, so this is their only completion.
pub fn command_handoff(program: &OsStr) {
    let Some(sink) = sink() else { return };
    let mut record = serde_json::Map::new();
    record.insert("name".to_owned(), sink.command.clone().into());
    record.insert("outcome".to_owned(), "exec".into());
    record.insert(
        "exec".to_owned(),
        Path::new(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    emit("cmd", millis(sink.started), record);
}

fn millis(started: Instant) -> u128 {
    started.elapsed().as_millis()
}

fn emit(kind: &str, ms: u128, mut record: serde_json::Map<String, serde_json::Value>) {
    let Some(sink) = sink() else { return };
    let mut head = serde_json::Map::new();
    head.insert("v".to_owned(), 1.into());
    head.insert(
        "t".to_owned(),
        crate::fsx::timestamp().unwrap_or_default().into(),
    );
    head.insert("run".to_owned(), sink.run.clone().into());
    head.insert(
        "seq".to_owned(),
        sink.seq.fetch_add(1, Ordering::Relaxed).into(),
    );
    head.insert("pid".to_owned(), std::process::id().into());
    head.insert("cmd".to_owned(), sink.command.clone().into());
    head.insert("kind".to_owned(), kind.into());
    head.append(&mut record);
    head.insert("ms".to_owned(), (ms as u64).into());

    let mut line = serde_json::Value::Object(head).to_string();
    if line.len() > MAX_RECORD - 1 {
        line.truncate(MAX_RECORD - 1);
    }
    line.push('\n');
    append(&sink.path, line.as_bytes());
}

fn append(path: &Path, line: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = crate::fsx::create_private_dir(parent);
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = file.write_all(line);
    }
}

fn rotate(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
}

fn run_id() -> String {
    crate::fsx::random_tree_id().map_or_else(
        |_| std::process::id().to_string(),
        |id| id.chars().take(8).collect(),
    )
}
