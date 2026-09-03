#![forbid(unsafe_code)]
#![deny(
    clippy::dbg_macro,
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout
)]

pub mod adapters;
pub mod address;
pub mod config;
pub mod coords;
pub mod declarations;
pub mod doctor;
pub mod drift;
pub mod env;
pub mod error;
pub mod exclude;
pub mod from_ref;
pub mod lifecycle;
pub mod model;
pub mod new;
pub mod ports;
pub mod remove;
pub mod render;
pub mod report;
pub mod resource;
pub mod session;
pub mod settings;
pub mod setup;
pub mod snapshot;
pub mod sweep;
pub mod task;
pub mod template;
pub mod tmuxconf;
pub mod tui;

pub use env::{assemble, deactivate};
pub use error::{CoreError, ErrorCode, ExitClass};
pub use lifecycle::{derive_phase, TreeObs};

pub mod init {
    pub use crate::lifecycle::init::*;
}
