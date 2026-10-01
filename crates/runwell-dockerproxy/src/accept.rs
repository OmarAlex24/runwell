use std::{future::Future, io, time::Duration};
use tokio_util::sync::CancellationToken;

pub(crate) async fn retry<T, F, Fut>(mut accept: F, cancel: &CancellationToken) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    loop {
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return None,
            result = accept() => result,
        };
        match result {
            Ok(stream) => return Some(stream),
            Err(error) => tracing::warn!(%error, "Docker proxy accept failed; retrying"),
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn transient_accept_errors_back_off_then_recover_and_cancel() {
        let cancel = CancellationToken::new();
        let mut failures = [
            Err(io::Error::from(rustix::io::Errno::MFILE)),
            Err(io::Error::from(rustix::io::Errno::INTR)),
            Ok(42),
        ]
        .into_iter();
        let start = tokio::time::Instant::now();
        assert_eq!(
            retry(|| std::future::ready(failures.next().unwrap()), &cancel).await,
            Some(42)
        );
        assert!(start.elapsed() >= Duration::from_millis(200));
        let stopped = cancel.clone();
        let result = retry(
            || {
                stopped.cancel();
                std::future::ready(Err::<(), _>(io::Error::from(rustix::io::Errno::MFILE)))
            },
            &cancel,
        )
        .await;
        assert_eq!(result, None);
    }
}
