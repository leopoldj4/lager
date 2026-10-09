//! rustgit: a Git server that keeps repositories in S3-compatible storage.
//!
//! See ARCHITECTURE.md for the design and IMPLEMENTATION_PLAN.md for the stages.

// HTTP handlers return complete responses as errors.
#![allow(clippy::result_large_err)]

pub mod backup;
pub mod catalog;
pub mod config;
pub mod git;
pub mod http;
pub mod identity;
pub mod maintenance;
pub mod metrics;
pub mod model;
pub mod repository;
pub mod routing;
pub mod server;
pub mod storage;
