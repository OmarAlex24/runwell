use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct Node {
    /// Validated runwell TOML configuration.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Wire scale-set controller logic directly to this local Linux node.
    #[arg(long)]
    standalone: bool,
    #[command(subcommand)]
    action: Option<Action>,
}
#[derive(Debug, Subcommand)]
enum Action {
    /// Persistently stop admission and finish current jobs.
    Drain,
    /// Resume admission after a completed upgrade.
    Resume,
}
impl Node {
    pub async fn run(self) -> Result<(), String> {
        let source = std::fs::read_to_string(self.config.ok_or("--config is required")?)
            .map_err(|_| "cannot read node configuration")?;
        let config = runwell_config::Config::from_toml(&source).map_err(|e| e.to_string())?;
        if self.action.is_some() {
            if self.standalone {
                return Err("use SIGTERM to drain standalone mode".into());
            }
            let db = config.node.state_dir.join("node-agent.sqlite");
            if !db.is_file() {
                return Err("node execution journal does not exist".into());
            }
            let store = runwell_store::Store::open(db.to_str().ok_or("invalid state path")?)
                .await
                .map_err(|e| e.to_string())?;
            return match self.action {
                Some(Action::Resume) => store.resume_node().await,
                _ => store.drain_node().await,
            }
            .map_err(|e| e.to_string());
        }
        let _ = tracing_subscriber::fmt()
            .with_env_filter("runwell_node=info,runwell_runner=info,runwell_controller=info")
            .try_init();
        #[cfg(not(target_os = "linux"))]
        {
            let _ = config;
            Err(runwell_node::Error::Unsupported.to_string())
        }
        #[cfg(target_os = "linux")]
        {
            if self.standalone {
                runwell_node::linux::standalone(config)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                super::node_network::run(config)
                    .await
                    .map_err(|e| e.to_string())
            }
        }
    }
}
