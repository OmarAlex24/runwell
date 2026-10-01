//! Controller-only GitHub credential and session setup.
use crate::{Controller, Fleet, GithubGateway, LogHooks};
use runwell_config::{AuthConfig, Config};
use runwell_node::{Drain, Error};
use runwell_scaleset::{ActionsClient, Credentials, Label, RunnerSetting, ScaleSet, Secret};
use runwell_transport::{Client, Identity, Rpc, Tls, WallClock};
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Run network controller with an exclusive journal lock and signal-safe sessions.
pub async fn controller(config: Config) -> Result<(), Error> {
    config.validate().map_err(|_| Error::Config)?;
    let settings = config.standalone.clone().ok_or(Error::Config)?;
    let network = config.network.as_ref().ok_or(Error::Config)?;
    let _lock = journal_lock(&config.controller.database)?;
    let store =
        runwell_store::Store::open(config.controller.database.to_str().ok_or(Error::Config)?)
            .await?;
    let tls =
        Tls::load(config.transport.as_ref().ok_or(Error::Config)?).map_err(|_| Error::Config)?;
    let client = ActionsClient::new(runwell_scaleset::Config::new(
        &config.github.config_url,
        credentials(&config.github.auth)?,
    )?)?;
    let gateway = Arc::new(GithubGateway::new(client.clone()));
    let mut classes = BTreeMap::new();
    for class in &config.controller.classes {
        let mut names = vec![class.name.clone()];
        for label in &class.labels {
            if !names.contains(label) {
                names.push(label.clone());
            }
        }
        let desired = ScaleSet {
            name: class.name.clone(),
            runner_group_id: settings.runner_group_id,
            labels: names
                .into_iter()
                .map(|name| Label {
                    kind: "System".into(),
                    name,
                })
                .collect(),
            runner_setting: RunnerSetting {
                disable_update: true,
            },
            ..Default::default()
        };
        let set = match client
            .get_scale_set_by_name(settings.runner_group_id, &class.name)
            .await?
        {
            Some(existing) => client.update_scale_set(existing.id, desired).await?,
            None => client.create_scale_set(desired).await?,
        };
        if set.id <= 0 || classes.insert(set.id, class.clone()).is_some() {
            return Err(Error::Config);
        }
    }

    let mut peers: BTreeMap<String, Arc<dyn Rpc>> = BTreeMap::new();
    let mut clients = Vec::new();
    for peer in &network.nodes {
        let client = Arc::new(
            Client::new(
                peer.address.clone(),
                Identity::node(&peer.id),
                &tls,
                Duration::from_secs(network.rpc_seconds),
                network.rpc_attempts,
            )
            .map_err(|_| Error::Config)?,
        );
        peers.insert(peer.id.clone(), client.clone());
        clients.push(client);
    }
    let policy = Arc::new(runwell_scheduler::Runwell {
        priority: runwell_scheduler::Priority::Fifo,
        aging_seconds: 300.0,
        admission: runwell_admission::ReservationAdmission::new(
            settings.overcommit.cpu,
            settings.overcommit.memory,
        )
        .map_err(|_| Error::Config)?,
    });
    let fleet = Arc::new(
        Fleet::new(
            config.clone(),
            store.clone(),
            peers,
            gateway.clone(),
            policy,
            Arc::new(WallClock),
            Arc::new(LogHooks),
        )?
        .with_release_updates()?,
    );
    let listener = tokio::net::TcpListener::bind(network.controller_listen).await?;
    let allowed = network
        .nodes
        .iter()
        .map(|n| Identity::node(&n.id))
        .collect();
    let stop = CancellationToken::new();
    let streams = crate::streams::subscribe(fleet.clone(), clients, stop.clone());
    let server_stop = stop.clone();
    let handler = fleet.clone();
    let server = tokio::spawn(async move {
        runwell_transport::serve(listener, &tls, allowed, handler, server_stop).await
    });
    let mut controller = Controller::new(&config, classes.clone(), store, fleet, gateway.clone())?;
    let (send, receive) = tokio::sync::mpsc::channel(2);
    let signals = signals(send)?;
    let result = async {
        // A partition cannot prevent recovery of unrelated hosts. Their durable
        // jobs remain until the periodic reconciliation can reach them again.
        if let Err(error) = controller.reconcile().await {
            tracing::warn!(%error, "startup recovery deferred for unavailable nodes");
        }
        for set in classes.keys() {
            gateway
                .open_session(*set, &network.controller_id, 0)
                .await?;
        }
        runwell_node::run_loop(&mut controller, gateway.clone(), receive).await
    }
    .await;
    signals.abort();
    stop.cancel();
    let _ = server.await;
    for task in streams {
        let _ = task.await;
    }
    result.and(gateway.close().await)
}
/// Restrict journal files before SQLite creates sidecars and hold an OS lock.
pub fn journal_lock(database: &std::path::Path) -> Result<fs::File, Error> {
    let parent = database.parent().ok_or(Error::Config)?;
    fs::create_dir_all(parent)?;
    let lock = private_file(&database.with_extension("lock"))?;
    lock.try_lock().map_err(|_| Error::Locked)?;
    private_file(database)?;
    Ok(lock)
}
fn private_file(path: &std::path::Path) -> Result<fs::File, Error> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
        return Err(Error::Config);
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
/// First signal drains; a second signal exits without stopping host units.
pub fn signals(
    send: tokio::sync::mpsc::Sender<Drain>,
) -> Result<tokio::task::JoinHandle<()>, Error> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        Ok(tokio::spawn(async move {
            let mut first = true;
            loop {
                tokio::select! { _ = term.recv() => {}, _ = interrupt.recv() => {} }
                let request = if first {
                    Drain::Graceful
                } else {
                    Drain::Immediate
                };
                first = false;
                if send.send(request).await.is_err() {
                    break;
                }
            }
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = send;
        Err(Error::Unsupported)
    }
}
fn credentials(config: &AuthConfig) -> Result<Credentials, Error> {
    let file = |path| fs::read_to_string(path).map_err(|_| Error::Config);
    let env = |name| std::env::var(name).map_err(|_| Error::Config);
    let secret = |value: String| -> Result<Secret, Error> {
        if value.trim().is_empty() {
            Err(Error::Config)
        } else {
            Ok(Secret::new(value.trim().to_owned()))
        }
    };
    match config {
        AuthConfig::Pat { token_file } => Ok(Credentials::Pat(secret(file(token_file)?)?)),
        AuthConfig::PatEnv { token_env } => Ok(Credentials::Pat(secret(env(token_env)?)?)),
        AuthConfig::App {
            app_id,
            installation_id,
            private_key_file,
        } => Ok(Credentials::App {
            client_id: app_id.to_string(),
            installation_id: *installation_id,
            private_key: secret(file(private_key_file)?)?,
        }),
        AuthConfig::AppEnv {
            app_id,
            installation_id,
            private_key_env,
        } => Ok(Credentials::App {
            client_id: app_id.to_string(),
            installation_id: *installation_id,
            private_key: secret(env(private_key_env)?)?,
        }),
    }
}
