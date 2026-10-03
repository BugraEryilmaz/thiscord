//! Explicit retry policy: authentication, permission and device failures are final.
use std::time::Duration;
use thiscord_shared::{ApiError, ErrorCode};

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub retryable: bool,
}
impl Failure {
    pub fn temporary(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
    pub fn server(error: ApiError) -> Self {
        // The current server reports a still-live connection as validation failure.
        let legacy_closing = error.code == ErrorCode::ValidationFailed
            && error.message == "Already connected to this voice channel";
        Self {
            retryable: legacy_closing
                || matches!(
                    error.code,
                    ErrorCode::Conflict
                        | ErrorCode::RateLimited
                        | ErrorCode::ServiceUnavailable
                        | ErrorCode::InternalError
                ),
            message: error.message,
        }
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
#[derive(Default)]
pub struct Backoff {
    failures: u32,
}
impl Backoff {
    pub fn next(&mut self, stable: bool, jitter: u32) -> Duration {
        if stable {
            self.failures = 0;
        }
        let seconds = (1_u64 << self.failures.min(5)).min(30);
        self.failures = self.failures.saturating_add(1);
        Duration::from_millis(seconds * 1000 + u64::from(jitter % 501))
    }
}
pub trait Connector: Send {
    fn attempt(&mut self) -> impl std::future::Future<Output = (Failure, bool)> + Send;
    fn waiting(&mut self, error: &Failure, delay: Duration);
}
/// Cancellation covers connection establishment, active media and backoff alike.
pub async fn supervise(
    connector: &mut impl Connector,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
    mut jitter: impl FnMut() -> u32 + Send,
) -> Option<Failure> {
    let retry = async {
        let mut backoff = Backoff::default();
        loop {
            let (error, stable) = connector.attempt().await;
            if !error.retryable {
                return error;
            }
            let delay = backoff.next(stable, jitter());
            connector.waiting(&error, delay);
            tokio::time::sleep(delay).await;
        }
    };
    tokio::select! { biased; _ = &mut stopped => None, error = retry => Some(error) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_back_off_are_bounded_and_stable_calls_reset_the_delay() {
        let mut backoff = Backoff::default();
        for expected in [1, 2, 4, 8, 16, 30, 30, 30] {
            assert_eq!(backoff.next(false, 0), Duration::from_secs(expected));
        }
        assert_eq!(backoff.next(true, 250), Duration::from_millis(1250));
        assert_eq!(backoff.next(false, 0), Duration::from_secs(2));
    }
    #[test]
    fn only_transient_server_errors_retry() {
        for code in [
            ErrorCode::Unauthorized,
            ErrorCode::Forbidden,
            ErrorCode::NotFound,
            ErrorCode::BadRequest,
            ErrorCode::ValidationFailed,
            ErrorCode::MethodNotAllowed,
            ErrorCode::Conflict,
            ErrorCode::RateLimited,
            ErrorCode::ServiceUnavailable,
            ErrorCode::InternalError,
        ] {
            let expected = matches!(
                code,
                ErrorCode::Conflict
                    | ErrorCode::RateLimited
                    | ErrorCode::ServiceUnavailable
                    | ErrorCode::InternalError
            );
            let error = ApiError {
                code,
                message: "failure".into(),
                request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
                fields: vec![],
            };
            assert_eq!(Failure::server(error).retryable, expected);
        }
        assert!(!Failure::from("device disconnected").retryable);
    }
    struct Fake {
        attempts: usize,
        waits: usize,
        cancel: Option<tokio::sync::oneshot::Sender<()>>,
        cancel_in_attempt: bool,
        terminal_first: bool,
    }
    impl Connector for Fake {
        async fn attempt(&mut self) -> (Failure, bool) {
            self.attempts += 1;
            if self.cancel_in_attempt {
                let _ = self.cancel.take().unwrap().send(());
                std::future::pending::<()>().await;
            }
            if self.terminal_first || self.attempts == 2 {
                (
                    Failure::from("permission denied after reauthentication"),
                    false,
                )
            } else {
                (Failure::temporary("connection lost"), false)
            }
        }
        fn waiting(&mut self, _: &Failure, _: Duration) {
            self.waits += 1;
            if let Some(cancel) = self.cancel.take() {
                let _ = cancel.send(());
            }
        }
    }
    fn fake() -> Fake {
        Fake {
            attempts: 0,
            waits: 0,
            cancel: None,
            cancel_in_attempt: false,
            terminal_first: false,
        }
    }
    #[tokio::test]
    async fn dropped_connection_retries_then_stops_on_denied_access() {
        let (_keep_alive, stopped) = tokio::sync::oneshot::channel();
        let mut connector = fake();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            supervise(&mut connector, stopped, || 0),
        )
        .await
        .unwrap();
        assert!(!result.unwrap().retryable);
        assert_eq!(connector.attempts, 2);
        assert_eq!(connector.waits, 1);
    }
    #[tokio::test]
    async fn leaving_cancels_both_backoff_and_pending_connection_establishment() {
        for cancel_in_attempt in [false, true] {
            let (cancel, stopped) = tokio::sync::oneshot::channel();
            let mut connector = Fake {
                cancel: Some(cancel),
                cancel_in_attempt,
                ..fake()
            };
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(200),
                    supervise(&mut connector, stopped, || 0)
                )
                .await
                .unwrap()
                .is_none()
            );
            assert_eq!(connector.attempts, 1);
        }
    }
    #[tokio::test]
    async fn permanent_failure_never_schedules_a_retry() {
        let (_keep_alive, stopped) = tokio::sync::oneshot::channel();
        let mut connector = Fake {
            terminal_first: true,
            ..fake()
        };
        assert!(supervise(&mut connector, stopped, || 0).await.is_some());
        assert_eq!(connector.attempts, 1);
        assert_eq!(connector.waits, 0);
    }
    #[test]
    fn only_the_exact_legacy_duplicate_join_error_gets_a_grace_retry() {
        let mut error = ApiError {
            code: ErrorCode::ValidationFailed,
            message: "Already connected to this voice channel".into(),
            request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            fields: vec![],
        };
        assert!(Failure::server(error.clone()).retryable);
        error.message = "Invalid voice request".into();
        assert!(!Failure::server(error).retryable);
    }
}
