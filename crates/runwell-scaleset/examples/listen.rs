//! Live opt-in: RUNWELL_LIVE=1, GITHUB_CONFIG_URL, GITHUB_TOKEN,
//! RUNWELL_SCALE_SET_ID and optionally RUNWELL_MAX_CAPACITY (default zero).
//! Prints batches, acquires offered jobs, then acknowledges. Supply a nonzero
//! capacity only when an external supervisor is ready to provision runners.
use futures_util::StreamExt;
use runwell_scaleset::{ActionsClient, Config, Credentials, Event, Listener, Secret};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("RUNWELL_LIVE").as_deref() != Ok("1") {
        println!("Set RUNWELL_LIVE=1 and the documented environment variables to connect.");
        return Ok(());
    }
    let config = Config::new(
        std::env::var("GITHUB_CONFIG_URL")?,
        Credentials::Pat(Secret::new(std::env::var("GITHUB_TOKEN")?)),
    )?;
    let id = std::env::var("RUNWELL_SCALE_SET_ID")?.parse()?;
    let capacity = std::env::var("RUNWELL_MAX_CAPACITY")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    let client = ActionsClient::new(config)?;
    let session = client
        .open_session(id, format!("runwell-{}", uuid::Uuid::new_v4()))
        .await?;
    let mut listener = Listener::new(session, capacity);
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        loop {
            tokio::select! {
                signal = &mut shutdown => { signal?; break; }
                message = listener.next() => {
                    let Some(message) = message else { break; };
                    let message = message?;
                    println!("{message:?}");
                    let ids: Vec<_> = message.events.iter().filter_map(|event| match event {
                        Event::JobAvailable(job) => Some(job.job.runner_request_id), _ => None,
                    }).collect();
                    if !ids.is_empty() { listener.acquire_jobs(&ids).await?; }
                    if let Some(id) = message.message_id { listener.ack(id).await?; }
                }
            }
        }
        Ok(())
    }
    .await;
    let closed = listener.close().await;
    result?;
    closed?;
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
