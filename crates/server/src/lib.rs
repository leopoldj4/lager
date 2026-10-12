//! worktree-server: the Git HTTP server that keeps repositories in
//! S3-compatible storage.
//!
//! See IMPLEMENTATION_PLAN.md for the stages.

// HTTP handlers return complete responses as errors.
#![allow(clippy::result_large_err)]

pub mod bus;
pub mod disk;
pub mod git;
pub mod http;
pub mod metrics;
pub mod proc_receive;
pub mod repository;
pub mod routing;
pub mod server;
pub mod snapshot;
pub mod web;
