//! Explicit retry policy: authentication, permission and device failures are final.
use std::time::Duration;
use thiscord_shared::{ApiError, ErrorCode};

/// After a restart notice, retry steadily (not exponentially) for this long:
/// the next server process is usually ready within seconds.
pub const RESTART_RETRY_WINDOW: Duration = Duration::from_secs(180);
const RESTART_RETRY_MS: u64 = 1_500;

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub retryable: bool,
    reason: &'static str,
    /// The server's first-retry hint from a restart notice.
    retry_after: Option<Duration>,
}
impl Failure {
    pub fn temporary(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            reason: "transport",
            retry_after: None,
        }
    }
    /// A planned server restart: rejoin with a fresh Join after `retry_after`.
    pub fn restarting(retry_after: Duration) -> Self {
        Self {
            message: "Thiscord is restarting".into(),
            retryable: true,
            reason: "server_restart",
            retry_after: Some(retry_after.min(Duration::from_secs(30))),
        }
    }
    /// A restart or a temporarily unavailable server: show a calm reconnecting
    /// state, not an error.
    pub fn calm(&self) -> bool {
        matches!(self.reason, "server_restart" | "server_unavailable")
    }
    pub fn is_restart(&self) -> bool {
        self.reason == "server_restart"
    }
    /// Log a fixed category, never a server body, device error or network URL.
    pub fn with_reason(mut self, reason: &'static str) -> Self {
        self.reason = reason;
        self
    }
    pub fn diagnostic_reason(&self) -> &'static str {
        self.reason
    }
    pub fn replaced() -> Self {
        Failure::from("This voice call was opened in another connection.").with_reason("replaced")
    }
    pub fn server(error: ApiError) -> Self {
        // Legacy servers reported a still-live connection as validation failure.
        let legacy_closing = error.code == ErrorCode::ValidationFailed
            && error.message == "Already connected to this voice channel";
        Self {
            retryable: matches!(
                error.code,
                ErrorCode::Conflict
                    | ErrorCode::RateLimited
                    | ErrorCode::ServiceUnavailable
                    | ErrorCode::InternalError
            ),
            reason: if legacy_closing {
                "duplicate_join"
            } else if error.code == ErrorCode::ServiceUnavailable {
                // A draining server refuses joins this way, as can a briefly
                // unavailable database.
                "server_unavailable"
            } else {
                "server"
            },
            message: error.message,
            retry_after: None,
        }
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            retryable: false,
            reason: "local_or_protocol",
            retry_after: None,
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
    restart_until: Option<std::time::Instant>,
}
impl Backoff {
    /// The delay before retrying after `error`. A restart notice waits for its
    /// hint, then retries steadily while the next server process starts; the
    /// exponential schedule resumes after `RESTART_RETRY_WINDOW`.
    pub fn after(&mut self, error: &Failure, stable: bool, jitter: u32) -> Duration {
        self.after_at(error, stable, jitter, std::time::Instant::now())
    }
    fn after_at(
        &mut self,
        error: &Failure,
        stable: bool,
        jitter: u32,
        now: std::time::Instant,
    ) -> Duration {
        if let Some(hint) = error.retry_after {
            self.restart_until = Some(now + RESTART_RETRY_WINDOW);
            self.failures = 0;
            return hint + Duration::from_millis(u64::from(jitter % 251));
        }
        if error.reason == "server_unavailable" && self.restart_until.is_none() {
            self.restart_until = Some(now + RESTART_RETRY_WINDOW);
        }
        if self.restart_until.is_some_and(|until| now < until) {
            return Duration::from_millis(RESTART_RETRY_MS + u64::from(jitter % 1_001));
        }
        self.restart_until = None;
        self.next(stable, jitter)
    }
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
            let delay = backoff.after(&error, stable, jitter());
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

    #[tokio::test]
    async fn replacement_is_terminal_and_never_enters_backoff() {
        struct Replaced;
        impl Connector for Replaced {
            async fn attempt(&mut self) -> (Failure, bool) {
                (Failure::replaced(), true)
            }
            fn waiting(&mut self, _: &Failure, _: Duration) {
                panic!("A displaced call must not retry");
            }
        }
        let (_keep_alive, stopped) = tokio::sync::oneshot::channel();
        let result = supervise(&mut Replaced, stopped, || 0).await.unwrap();
        assert!(!result.retryable);
        assert_eq!(result.diagnostic_reason(), "replaced");
    }
    #[test]
    fn legacy_duplicate_join_is_terminal_to_prevent_competing_retries() {
        let mut error = ApiError {
            code: ErrorCode::ValidationFailed,
            message: "Already connected to this voice channel".into(),
            request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            fields: vec![],
        };
        let duplicate = Failure::server(error.clone());
        assert!(!duplicate.retryable);
        assert_eq!(duplicate.diagnostic_reason(), "duplicate_join");
        error.message = "Invalid voice request".into();
        assert!(!Failure::server(error).retryable);
    }

    #[test]
    fn diagnostic_categories_do_not_include_dynamic_error_text() {
        let private = "private device path, token or server body";
        let errors = [
            Failure::from(private),
            Failure::temporary(private),
            Failure::temporary(private).with_reason("media_send_timeout"),
            Failure::server(ApiError {
                code: ErrorCode::InternalError,
                message: private.into(),
                request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
                fields: vec![],
            }),
        ];
        for (error, expected) in errors.into_iter().zip([
            "local_or_protocol",
            "transport",
            "media_send_timeout",
            "server",
        ]) {
            assert_eq!(error.diagnostic_reason(), expected);
            assert!(!error.diagnostic_reason().contains(private));
        }
    }

    #[test]
    fn restarts_wait_for_the_hint_then_retry_steadily_before_backing_off() {
        let start = std::time::Instant::now();
        let mut backoff = Backoff::default();
        // Ordinary failures before the restart grow the delay.
        backoff.next(false, 0);
        backoff.next(false, 0);
        let restart = Failure::restarting(Duration::from_millis(2_000));
        assert!(restart.retryable && restart.calm() && restart.is_restart());
        assert_eq!(
            backoff.after_at(&restart, false, 100, start),
            Duration::from_millis(2_100)
        );
        // While the next process starts, refused connections retry every ~2 s.
        let down = Failure::temporary("connection refused");
        for second in [3, 10, 60, 179] {
            let delay = backoff.after_at(&down, false, 0, start + Duration::from_secs(second));
            assert_eq!(delay, Duration::from_millis(1_500));
        }
        // Past the window the ordinary bounded schedule resumes from the start.
        let late = start + RESTART_RETRY_WINDOW + Duration::from_secs(1);
        assert_eq!(
            backoff.after_at(&down, false, 0, late),
            Duration::from_secs(1)
        );
        assert_eq!(
            backoff.after_at(&down, false, 0, late),
            Duration::from_secs(2)
        );
        // Hints are bounded; a draining server's refusal is calm as well.
        assert_eq!(
            Failure::restarting(Duration::from_secs(600)).retry_after,
            Some(Duration::from_secs(30))
        );
        let unavailable = Failure::server(ApiError {
            code: ErrorCode::ServiceUnavailable,
            message: "Thiscord is restarting. Reconnecting shortly.".into(),
            request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            fields: vec![],
        });
        assert!(unavailable.retryable && unavailable.calm() && !unavailable.is_restart());
        let mut fresh = Backoff::default();
        assert_eq!(
            fresh.after_at(&unavailable, false, 0, start),
            Duration::from_millis(1_500)
        );
        assert!(!Failure::replaced().calm());
    }
}
