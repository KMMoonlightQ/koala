use crate::llm::LlmError;
use std::time::Duration;

/// Retry only connection-phase failures: transport errors, 429 and 5xx.
/// Mid-stream failures are not retryable (partial output may already exist).
pub fn is_retryable(err: &LlmError) -> bool {
    match err {
        LlmError::Http(_) => true,
        LlmError::Api { status, .. } => *status == 429 || *status >= 500,
        _ => false,
    }
}

/// Exponential backoff with jitter: 1s, 2s, 4s, ... (one entry per retry).
pub fn backoff_delays(max_retries: usize) -> Vec<Duration> {
    (0..max_retries)
        .map(|i| {
            let base_ms = 1000u64 << i.min(5);
            let jitter = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_millis() as u64)
                .unwrap_or(0))
                % 500;
            Duration::from_millis(base_ms + jitter)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(is_retryable(&LlmError::Api {
            status: 429,
            body: String::new()
        }));
        assert!(is_retryable(&LlmError::Api {
            status: 503,
            body: String::new()
        }));
        assert!(!is_retryable(&LlmError::Api {
            status: 400,
            body: String::new()
        }));
        assert!(!is_retryable(&LlmError::EmptyResponse));
    }

    #[test]
    fn backoff_grows_exponentially() {
        let delays = backoff_delays(3);
        assert_eq!(delays.len(), 3);
        assert!(
            delays[0] >= Duration::from_millis(1000) && delays[0] < Duration::from_millis(1500)
        );
        assert!(
            delays[1] >= Duration::from_millis(2000) && delays[1] < Duration::from_millis(2500)
        );
        assert!(
            delays[2] >= Duration::from_millis(4000) && delays[2] < Duration::from_millis(4500)
        );
    }
}
