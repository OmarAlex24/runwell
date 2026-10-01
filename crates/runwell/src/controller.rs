use clap::Args;
use std::path::PathBuf;
#[derive(Debug, Args)]
pub struct Controller {
    #[arg(long)]
    config: PathBuf,
}
impl Controller {
    pub async fn run(self) -> Result<(), String> {
        let source = std::fs::read_to_string(self.config)
            .map_err(|_| "cannot read controller configuration")?;
        let config = runwell_config::Config::from_toml(&source).map_err(|e| e.to_string())?;
        let _ = tracing_subscriber::fmt()
            .with_env_filter("runwell=info,runwell_node=info,runwell_controller=info")
            .try_init();
        runwell_controller::bootstrap::controller(config)
            .await
            .map_err(|e| e.to_string())
    }
}
