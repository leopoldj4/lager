//! worktree-server: the Git HTTP server that keeps repositories in
//! S3-compatible storage.
//!
//! See ARCHITECTURE.md for the design and IMPLEMENTATION_PLAN.md for the stages.

// HTTP handlers return complete responses as errors.
#![allow(clippy::result_large_err)]

pub mod http;
pub mod proc_receive;
pub mod routing;
pub mod server;
