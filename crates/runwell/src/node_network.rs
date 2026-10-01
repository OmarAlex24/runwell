use runwell_node::Error;
use runwell_transport::{Agent, Client, Identity, Tls, WallClock};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub async fn run(config: runwell_config::Config) -> Result<(), Error> {
    runwell_node::linux::require_root()?;
    let settings = config.standalone.clone().ok_or(Error::Config)?;
    let network = config.network.as_ref().ok_or(Error::Config)?;
    let _host_lock =
        runwell_controller::bootstrap::journal_lock(std::path::Path::new("/run/runwell-node"))?;
    let database = config.node.state_dir.join("node-agent.sqlite");
    let _state_lock = runwell_controller::bootstrap::journal_lock(&database)?;
    let store = runwell_store::Store::open(database.to_str().ok_or(Error::Config)?).await?;
    let tls =
        Tls::load(config.transport.as_ref().ok_or(Error::Config)?).map_err(|_| Error::Config)?;
    let controller = Client::new(
        network.controller_address.clone(),
        Identity::controller(&network.controller_id),
        &tls,
        Duration::from_secs(network.rpc_seconds),
        network.rpc_attempts,
    )
    .map_err(|_| Error::Config)?;
    let backend = Arc::new(
        runwell_node::linux::LinuxBackend::connect(settings.clone())
            .await?
            .with_docker_proxy(config.node.id.clone()),
    );
    let agent = Arc::new(
        Agent::open(config.clone(), store.clone(), backend, Arc::new(WallClock))
            .await
            .map_err(|_| Error::Config)?,
    );
    let listener = tokio::net::TcpListener::bind(network.node_listen).await?;
    let allowed = [Identity::controller(&network.controller_id)].into();
    let stop = CancellationToken::new();
    let server_stop = stop.clone();
    let handler = agent.clone();
    let mut server = tokio::spawn(async move {
        runwell_transport::serve(listener, &tls, allowed, handler, server_stop).await
    });
    let (send, signals) = tokio::sync::mpsc::channel(2);
    let signal_task = runwell_controller::bootstrap::signals(send)?;
    let result = tokio::select! {
        result = &mut server => result.map_err(|_| Error::Io)?.map_err(|_| Error::Io),
        result = runwell_transport::report_until_drained(
            agent.as_ref(), &controller, &store, signals,
            Duration::from_secs(network.report_seconds),
            Duration::from_secs(settings.drain_seconds),
        ) => result.map_err(|_| Error::Io),
    };
    signal_task.abort();
    stop.cancel();
    if !server.is_finished() {
        let _ = server.await;
    }
    result
}
