//! Thin, effectful platform integration for `wt`.
// The workspace-root lint blocks effects in the binary; this crate is their
// sole implementation boundary (SPEC §15).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod failpoint;
pub mod fsx;
pub mod git;
pub mod lock;
pub mod net;
pub mod proc;
pub mod snapshot;
pub mod tmux;

pub type Result<T> = std::result::Result<T, wt_core::CoreError>;
