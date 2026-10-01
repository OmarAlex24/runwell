use crate::NodeFuture;

/// Evaluate every local teardown step, preserving the first error for reconcile.
pub(crate) async fn finish<'a>(
    mut result: Result<(), crate::Error>,
    steps: impl IntoIterator<Item = NodeFuture<'a, ()>>,
) -> Result<(), crate::Error> {
    for step in steps {
        let next = step.await;
        result = result.and(next);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn proxy_failure_still_stops_slice_and_removes_credentials_and_install() {
        let performed = Arc::new(Mutex::new(Vec::new()));
        let steps = ["slice", "credentials", "install"].into_iter().map(|name| {
            let performed = performed.clone();
            Box::pin(async move {
                performed.lock().unwrap().push(name);
                if name == "credentials" {
                    Err(Error::Io)
                } else {
                    Ok(())
                }
            }) as NodeFuture<'_, ()>
        });
        // Timeout stands in for an unavailable external cleanup service, keeping
        // this orchestration regression portable without Linux/Docker dependencies.
        assert!(matches!(
            finish(Err(Error::Timeout), steps).await,
            Err(Error::Timeout)
        ));
        assert_eq!(
            *performed.lock().unwrap(),
            ["slice", "credentials", "install"]
        );
    }
}
