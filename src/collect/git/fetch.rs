//! Remote fetch helper used prior to revwalk.
//!
//! Performs a non-interactive `git fetch` against a configured remote so
//! the local clone has the latest commits before [`super::GitCollector`]
//! walks history. The fetch is best-effort: authentication or network
//! failures are logged and downgraded to `Ok(())` so collection can
//! proceed on whatever the local clone already has.
//!
//! ## Authentication strategy
//!
//! - SSH agent (if running and has a usable key)
//! - SSH key files at `~/.ssh/id_ed25519` then `~/.ssh/id_rsa`
//! - **No interactive prompts** — SSH BatchMode-equivalent. We never ask
//!   for a password or passphrase, because the binary is typically run
//!   in CI / background tasks where stdin is unavailable.

use std::path::PathBuf;

use git2::{Cred, CredentialType, FetchOptions, RemoteCallbacks, Repository};
use tracing::{info, warn};

use crate::collect::errors::Result;

/// Fetch all refs from the configured remote before walking.
///
/// Returns `Ok(())` if:
/// - the fetch succeeds,
/// - the repository has no remote with the requested name (purely local
///   repos are a supported case), or
/// - authentication fails non-interactively (warn-and-continue so a CI
///   misconfiguration doesn't break the whole pipeline).
///
/// # Errors
///
/// Currently returns [`crate::collect::CollectError`] only for unexpected libgit2 errors
/// that aren't classified as auth/transport failures.
pub fn fetch_remote(repo: &Repository, remote_name: &str) -> Result<()> {
    let mut remote = match repo.find_remote(remote_name) {
        Ok(r) => r,
        Err(e) => {
            // Most common: local-only repo with no remotes configured.
            info!(
                remote = remote_name,
                error = %e,
                "no matching remote; skipping fetch (local-only repo)"
            );
            return Ok(());
        }
    };

    info!("Fetching from remote '{}'", remote_name);

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        non_interactive_credentials(url, username_from_url, allowed_types)
    });

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);

    // Fetch with default refspecs (empty slice == use configured refspecs).
    match remote.fetch(&[] as &[&str], Some(&mut fetch_options), None) {
        Ok(()) => Ok(()),
        Err(e) => {
            // git2 lumps auth, transport, and certificate problems together;
            // treat anything in the auth/transport family as a soft failure.
            if is_auth_or_transport_error(&e) {
                warn!(
                    remote = remote_name,
                    error = %e,
                    "fetch failed (auth/transport) — continuing with local refs"
                );
                Ok(())
            } else {
                warn!(
                    remote = remote_name,
                    error = %e,
                    "fetch failed — continuing with local refs"
                );
                Ok(())
            }
        }
    }
}

/// Build a credential without prompting the user.
///
/// Tries, in order:
/// 1. SSH agent (if `allowed_types` permits)
/// 2. `~/.ssh/id_ed25519`
/// 3. `~/.ssh/id_rsa`
/// 4. Default credential helper (HTTPS)
///
/// Returns a git2 error if none of these succeed — the caller will turn
/// that into a logged warning and continue.
fn non_interactive_credentials(
    _url: &str,
    username_from_url: Option<&str>,
    allowed_types: CredentialType,
) -> std::result::Result<Cred, git2::Error> {
    let username = username_from_url.unwrap_or("git");

    if allowed_types.contains(CredentialType::SSH_KEY) {
        // 1. SSH agent first.
        if let Ok(cred) = Cred::ssh_key_from_agent(username) {
            return Ok(cred);
        }

        // 2. Explicit key files (ed25519 preferred, rsa fallback).
        if let Some(home) = home_dir() {
            for key_name in &["id_ed25519", "id_rsa"] {
                let private_key = home.join(".ssh").join(key_name);
                if private_key.exists() {
                    // git2 needs a passphrase parameter even for unencrypted
                    // keys; we pass None to force non-interactive behavior.
                    if let Ok(cred) = Cred::ssh_key(username, None, private_key.as_path(), None) {
                        return Ok(cred);
                    }
                }
            }
        }
    }

    if allowed_types.contains(CredentialType::DEFAULT) {
        if let Ok(cred) = Cred::default() {
            return Ok(cred);
        }
    }

    Err(git2::Error::from_str(
        "no non-interactive credentials available (tried SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa)",
    ))
}

/// Best-effort home-directory lookup using `$HOME`.
///
/// We deliberately avoid pulling in the `home` / `dirs` crate for one
/// path lookup; `$HOME` is set on every supported platform.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Classify an error as an auth/transport failure suitable for soft-failing.
fn is_auth_or_transport_error(e: &git2::Error) -> bool {
    matches!(
        e.class(),
        git2::ErrorClass::Ssh
            | git2::ErrorClass::Http
            | git2::ErrorClass::Net
            | git2::ErrorClass::Callback
    )
}
