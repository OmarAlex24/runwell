use super::{LinuxBackend, require_root};
use crate::{Controller, Drain, Error, GithubGateway, NodeBackend, run_loop};
use runwell_config::{AuthConfig, Config};
use runwell_scaleset::{ActionsClient, Credentials, Label, RunnerSetting, ScaleSet, Secret};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    sync::Arc,
};

/// Launch standalone operation after validation, with an exclusive host/state
/// lock, startup reconciliation, one session per class, and Unix signal drain.
pub async fn standalone(config: Config) -> Result<(), Error> {
    require_root()?;
    config.validate().map_err(|_| Error::Config)?;
    let settings = config.standalone.clone().ok_or(Error::Config)?;
    let _host_lock = lock(std::path::Path::new("/run/runwell-node.lock"))?;
    fs::create_dir_all(&config.node.state_dir)?;
    if fs::symlink_metadata(&config.node.state_dir)?.is_symlink()
        || fs::metadata(&config.node.state_dir)?.uid() != 0
    {
        return Err(Error::Config);
    }
    fs::set_permissions(&config.node.state_dir, fs::Permissions::from_mode(0o711))?;
    let _state_lock = lock(&config.node.state_dir.join("node.lock"))?;
    let database = &config.controller.database;
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent)?;
    }
    let database_file = private_file(database)?;
    database_file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let store = runwell_store::Store::open(database.to_str().ok_or(Error::Config)?).await?;
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
    let backend = Arc::new(
        LinuxBackend::connect(settings)
            .await?
            .with_docker_proxy(config.node.id.clone()),
    );
    let mut controller = Controller::new(
        &config,
        classes.clone(),
        store,
        backend.clone(),
        gateway.clone(),
    )?;
    controller.reconcile().await?;
    backend.template_version().await?;
    let (signals, receive) = tokio::sync::mpsc::channel(2);
    // Install handlers before opening sessions, preserving any signal received
    // during setup. The second signal preserves live jobs and closes immediately.
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let signal_task = tokio::spawn(async move {
        let mut first = true;
        loop {
            tokio::select! { _ = term.recv() => {}, _ = interrupt.recv() => {} }
            let request = if first {
                Drain::Graceful
            } else {
                Drain::Immediate
            };
            first = false;
            if signals.send(request).await.is_err() {
                break;
            }
        }
    });
    let result = async {
        for set in classes.keys() {
            gateway.open_session(*set, &config.node.id, 0).await?;
        }
        run_loop(&mut controller, gateway.clone(), receive).await
    }
    .await;
    signal_task.abort();
    let closed = gateway.close().await;
    result.and(closed)
}
fn lock(path: &std::path::Path) -> Result<fs::File, Error> {
    let file = private_file(path)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| Error::Locked)?;
    Ok(file)
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

fn private_file(path: &std::path::Path) -> Result<fs::File, Error> {
    use rustix::fs::{Mode, OFlags, open};
    let fd = open(
        path,
        OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| Error::Io)?;
    let file = fs::File::from(fd);
    if file.metadata()?.uid() != 0 {
        return Err(Error::Config);
    }
    Ok(file)
}
