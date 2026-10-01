use clap::{Args, Subcommand};
use runwell_transport::{Identity, certs};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct Certs {
    #[command(subcommand)]
    command: Action,
}
#[derive(Debug, Subcommand)]
enum Action {
    /// Create an offline P-256 certificate authority; refuses overwrites.
    Ca {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 3650)]
        days: u32,
    },
    /// Issue a peer certificate. Rotate by choosing a fresh output directory.
    Issue {
        #[arg(long)]
        ca: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_parser = ["controller", "node"])]
        role: String,
        #[arg(long)]
        id: String,
        #[arg(long, default_value_t = 90)]
        days: u32,
    },
}
impl Certs {
    pub fn run(self) -> Result<(), String> {
        match self.command {
            Action::Ca { out, days } => certs::create_ca(&out, days),
            Action::Issue {
                ca,
                out,
                role,
                id,
                days,
            } => certs::issue(&ca, &out, &Identity { role, id }, days),
        }
        .map_err(|e| e.to_string())
    }
}
