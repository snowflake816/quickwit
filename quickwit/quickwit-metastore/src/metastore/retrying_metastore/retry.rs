// Copyright (C) 2022 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::fmt::Debug;
use std::time::Duration;

use futures::Future;
use rand::Rng;
use tracing::{debug, warn};

const DEFAULT_MAX_RETRY_ATTEMPTS: usize = 30;
const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(if cfg!(test) { 1 } else { 250 });
const DEFAULT_MAX_DELAY: Duration = Duration::from_millis(if cfg!(test) { 1 } else { 20_000 });

pub trait Retryable {
    fn is_retryable(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct RetryParams {
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: usize,
}

impl Default for RetryParams {
    fn default() -> Self {
        Self {
            base_delay: DEFAULT_BASE_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            max_attempts: DEFAULT_MAX_RETRY_ATTEMPTS,
        }
    }
}

/// Retry with exponential backoff and full jitter. Implementation and default values originate from
/// the Java SDK. See also: <https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/>.
pub async fn retry<F, U, E, Fut>(retry_params: &RetryParams, f: F) -> Result<U, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<U, E>>,
    E: Retryable + Debug + 'static,
{
    let mut attempt_count = 0;

    loop {
        let response = f().await;

        attempt_count += 1;

        match response {
            Ok(response) => {
                return Ok(response);
            }
            Err(error) => {
                if !error.is_retryable() {
                    return Err(error);
                }
                if attempt_count >= retry_params.max_attempts {
                    warn!(
                        attempt_count = %attempt_count,
                        "Request failed"
                    );
                    return Err(error);
                }

                let ceiling_ms = (retry_params.base_delay.as_millis() as u64
                    * 2u64.pow(attempt_count as u32))
                .min(retry_params.max_delay.as_millis() as u64);
                let delay_ms = rand::thread_rng().gen_range(0..ceiling_ms);
                debug!(
                    attempt_count = %attempt_count,
                    delay_ms = %delay_ms,
                    error = ?error,
                    "Request failed, retrying"
                );

                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use futures::future::ready;

    use super::{retry, RetryParams, Retryable};

    #[derive(Debug, Eq, PartialEq)]
    pub enum Retry<E> {
        Transient(E),
        Permanent(E),
    }

    impl<E> Retryable for Retry<E> {
        fn is_retryable(&self) -> bool {
            match self {
                Retry::Transient(_) => true,
                Retry::Permanent(_) => false,
            }
        }
    }

    async fn simulate_retries<T>(values: Vec<Result<T, Retry<usize>>>) -> Result<T, Retry<usize>> {
        let values_it = RwLock::new(values.into_iter());
        retry(&RetryParams::default(), || {
            ready(values_it.write().unwrap().next().unwrap())
        })
        .await
    }

    #[tokio::test]
    async fn test_retry_accepts_ok() {
        assert_eq!(simulate_retries(vec![Ok(())]).await, Ok(()));
    }

    #[tokio::test]
    async fn test_retry_does_retry() {
        assert_eq!(
            simulate_retries(vec![Err(Retry::Transient(1)), Ok(())]).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn test_retry_stops_retrying_on_non_retryable_error() {
        assert_eq!(
            simulate_retries(vec![Err(Retry::Permanent(1)), Ok(())]).await,
            Err(Retry::Permanent(1))
        );
    }

    #[tokio::test]
    async fn test_retry_retries_up_at_most_attempts_times() {
        let retry_sequence: Vec<_> = (0..30)
            .map(|retry_id| Err(Retry::Transient(retry_id)))
            .chain(Some(Ok(())))
            .collect();
        assert_eq!(
            simulate_retries(retry_sequence).await,
            Err(Retry::Transient(29))
        );
    }

    #[tokio::test]
    async fn test_retry_retries_up_to_max_attempts_times() {
        let retry_sequence: Vec<_> = (0..29)
            .map(|retry_id| Err(Retry::Transient(retry_id)))
            .chain(Some(Ok(())))
            .collect();
        assert_eq!(simulate_retries(retry_sequence).await, Ok(()));
    }
}
