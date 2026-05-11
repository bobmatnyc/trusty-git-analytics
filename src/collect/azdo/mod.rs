//! Azure DevOps integration (Phase 2).
//!
//! Phase 2 wires a real `reqwest` HTTP session with PAT-based Basic auth
//! and implements two endpoints against `api-version=7.1`:
//!
//! * `GET _apis/connectionData` — auth probe + identity echo
//!   ([`AzureDevOpsClient::test_connection`])
//! * `GET _apis/projects` — list projects (single page, up to 100)
//!   ([`AzureDevOpsClient::get_projects`])
//!
//! Phase 6 will add work-item fetching on top of this session.

pub mod client;

pub use client::{AzdoConnectionInfo, AzdoError, AzdoProject, AzureDevOpsClient, WorkItem};
