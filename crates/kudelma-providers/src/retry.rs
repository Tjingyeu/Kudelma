use std::time::Duration;

use reqwest::RequestBuilder;
use tokio::time::sleep;

use crate::cancel::CancelToken;
use crate::error::ProviderError;

const MAX_ATTEMPTS: u32 = 4;
const BASE_DELAY_MS: u64 = 800;

/// Send a request, retrying transport failures and rate-limit/server statuses.
/// The closure builds a fresh builder so the body can be replayed.
pub async fn send_with_retry(
    provider: &str,
    cancel: &CancelToken,
    mut make: impl FnMut() -> RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    let mut last_error = None;
    for attempt in 0..MAX_ATTEMPTS {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let result = make().send().await;
        match result {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok());
                let body = response.text().await.unwrap_or_default();
                let retryable = status.as_u16() == 429 || status.is_server_error();
                last_error = Some(ProviderError::Status {
                    provider: provider.into(),
                    status: status.as_u16(),
                    body: truncate(&body, 800),
                });
                if !retryable || attempt + 1 == MAX_ATTEMPTS {
                    break;
                }
                sleep_with_cancel(
                    Duration::from_millis(
                        retry_after
                            .map(|secs| secs.saturating_mul(1000))
                            .unwrap_or(BASE_DELAY_MS * 2u64.pow(attempt)),
                    ),
                    cancel,
                )
                .await?;
            }
            Err(error) => {
                let retryable = error.is_timeout() || error.is_connect();
                let message = error.to_string();
                if retryable {
                    last_error = Some(ProviderError::Other(message));
                    if attempt + 1 < MAX_ATTEMPTS {
                        sleep_with_cancel(
                            Duration::from_millis(BASE_DELAY_MS * 2u64.pow(attempt)),
                            cancel,
                        )
                        .await?;
                        continue;
                    }
                }
                return Err(ProviderError::Http(error));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| ProviderError::Other("request failed".into())))
}

async fn sleep_with_cancel(duration: Duration, cancel: &CancelToken) -> Result<(), ProviderError> {
    tokio::select! {
        _ = sleep(duration) => Ok(()),
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
