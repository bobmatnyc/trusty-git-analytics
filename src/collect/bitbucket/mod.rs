//! Bitbucket Cloud REST API client for pull-request metadata.
//!
//! Surfaces a single [`BitbucketClient`] that implements
//! [`crate::collect::pr_provider::PrProvider`], so the pipeline can use it
//! interchangeably with the GitHub client.

pub mod client;
pub mod types;

pub use client::BitbucketClient;
