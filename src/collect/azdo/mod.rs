//! Azure DevOps integration (Phase 1 stub).
//!
//! Phase 1 establishes the surface API: a non-network client that validates
//! credentials format and exposes `NotImplemented` errors tagged with the
//! phase in which each method will land. No HTTP calls are made.
//!
//! Phase 2 will add the HTTP session and a `GET _apis/connectionData`
//! auth probe; Phase 6 will add work-item fetching.

pub mod client;

pub use client::{AzdoConnectionInfo, AzdoError, AzdoProject, AzureDevOpsClient, WorkItem};
