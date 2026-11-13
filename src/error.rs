use std::path::Path;

/// Everything that can go wrong, with messages written for the person who
/// has to act on them.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configuration file is malformed or uses keys wt doesn't know.
    #[error("{0}")]
    Config(String),
    /// A named tree, repository or pool doesn't exist.
    #[error("{0}")]
    NotFound(String),
    /// The request conflicts with what already exists.
    #[error("{0}")]
    Conflict(String),
    /// wt's own state can't be read or was written by a newer version.
    #[error("{0}")]
    State(String),
    /// Every slot of a pool stayed leased for as long as the pool waits.
    #[error("{0}")]
    Busy(String),
    /// A command from a pool's config, such as `acquire`, failed.
    #[error("{0}")]
    Command(String),
    #[error("git {args}: {message}")]
    Git { args: String, message: String },
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Adapts an `io::Error` into one that says what wt was doing.
pub(crate) fn io_at(what: &str, path: &Path) -> impl FnOnce(std::io::Error) -> Error {
    let context = format!("{what} {}", path.display());
    move |source| Error::Io { context, source }
}
