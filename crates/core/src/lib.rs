//! The parts of worktree that the server and the command line share:
//! configuration, fixed limits, the stored data model, storage, the catalog,
//! repository settings, identities, pull requests, GitHub mirrors,
//! and the storage-only operations of the administration commands.
//!
//! See IMPLEMENTATION_PLAN.md for the stages.

pub mod admin;
pub mod backup;
pub mod cas;
pub mod catalog;
pub mod cleanup;
pub mod config;
pub mod github;
pub mod history;
pub mod identity;
pub mod limits;
pub mod model;
pub mod password;
pub mod pulls;
pub mod repo;
pub mod storage;
