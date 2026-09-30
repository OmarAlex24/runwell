use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct Node {
    /// Validated runwell TOML configuration.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Wire scale-set controller logic directly to this local Linux node.
    #[arg(long)]
    standalone: bool,
}
impl Node {
    pub async fn run(self) -> Result<(), String> {
        if !self.standalone {
            return Err(
                "networked node mode is not implemented; use --config runwell.toml --standalone"
                    .into(),
            );
        }
        #[cfg(target_os = "linux")]
        runwell_node::linux::require_root().map_err(|e| e.to_string())?;
        #[cfg(not(target_os = "linux"))]
        return Err(runwell_node::Error::Unsupported.to_string());
        #[cfg(target_os = "linux")]
        {
            let path = self.config.ok_or("--config is required for --standalone")?;
            let source =
                std::fs::read_to_string(path).map_err(|_| "cannot read node configuration")?;
            let config = runwell_config::Config::from_toml(&source).map_err(|e| e.to_string())?;
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new(
                    "runwell_node=info,runwell_runner=info,runwell_scaleset=info",
                ))
                .try_init();
            runwell_node::linux::standalone(config)
                .await
                .map_err(|e| e.to_string())
        }
    }
}
