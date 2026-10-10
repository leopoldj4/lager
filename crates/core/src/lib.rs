//! The parts of worktree that the server and the command line share:
//! configuration, the stored data model, storage, the catalog, identities,
//! and the repository engine that keeps a Git cache at the storage head.
//!
//! See IMPLEMENTATION_PLAN.md for the stages.

pub mod backup;
pub mod catalog;
pub mod config;
pub mod disk;
pub mod git;
pub mod identity;
pub mod maintenance;
pub mod metrics;
pub mod model;
pub mod password;
pub mod repository;
pub mod storage;
