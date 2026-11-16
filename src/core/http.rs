//! Shared `reqwest` client construction (#255).
//!
//! Why: each HTTP client used to pick its own timeouts by copy-paste, none set
//! a connect timeout, and several classify clients used `Client::new()` with
//! no timeout at all, so a stalled server could hang a run.
//! What: one builder that every tga HTTP client starts from, with a bounded
//! connect timeout and a bounded total request timeout.

use std::time::Duration;

use reqwest::{Client, ClientBuilder};

/// Bound on establishing a TCP/TLS connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total per-request bound for REST/GraphQL API calls.
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Total per-request bound for LLM completion calls, which legitimately take
/// longer than a REST lookup.
pub const LLM_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// A builder with [`CONNECT_TIMEOUT`] and the given total request timeout.
///
/// Why: the one place tga's HTTP timeouts are set (#255).
/// What: callers add headers / user agent and call `build()`.
/// Test: `core::http::tests::stalled_server_times_out_within_bound`.
pub fn client_builder(total: Duration) -> ClientBuilder {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(total)
}

/// A built client with [`CONNECT_TIMEOUT`] and the given total timeout.
///
/// Why: callers with no headers to add need a one-call constructor.
/// What: fails closed — a build error is returned, never replaced by an
/// untimed `Client::new()` (#255, matching #111 in `jev.rs`).
/// Test: `core::http::tests::stalled_server_times_out_within_bound`.
///
/// # Errors
///
/// The underlying `reqwest` builder fails (e.g. the TLS backend cannot
/// initialise).
pub fn timed_client(total: Duration) -> Result<Client, reqwest::Error> {
    client_builder(total).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::net::TcpListener;

    /// Why: proves the shared builder bounds a request to a server that
    /// accepts the connection and never answers.
    /// What: a local listener accepts and holds the socket; a client with a
    /// 1 s total timeout must return a timeout error well inside 10 s.
    #[tokio::test]
    async fn stalled_server_times_out_within_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let client = timed_client(Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        let res = tokio::time::timeout(
            Duration::from_secs(10),
            client.get(format!("http://{addr}/")).send(),
        )
        .await
        .expect("client must time out on its own before the 10 s guard");
        let err = res.expect_err("stalled server must yield an error");
        assert!(err.is_timeout(), "expected timeout error, got {err}");
        assert!(start.elapsed() < Duration::from_secs(5));
        hold.abort();
    }
}
