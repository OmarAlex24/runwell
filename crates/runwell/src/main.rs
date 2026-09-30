//! Single-binary entry point for runwell controller, node, and analysis commands.
//!
//! Report analyzes CI history and setup provides read-only host discovery;
//! other unfinished commands report their status and exit with code 2.

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Capacity-aware ephemeral GitHub Actions runners (pre-alpha)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the scale-set controller and scheduler.
    Controller,
    /// Run a Linux host execution node.
    Node,
    /// Explain CI latency using GitHub workflow history.
    Report(Box<runwell_report::ReportArgs>),
    /// Replay a recorded job trace against scheduling policies.
    Simulate,
    /// Suggest workflow improvements and agent-facing rules.
    Advise,
    /// Discover Linux hosts and start a resumable CI setup wizard.
    Setup(runwell_setup::SetupArgs),
    /// Print the runwell version.
    Version,
}

#[tokio::main]
async fn main() -> ExitCode {
    dispatch(Cli::parse().command).await
}

async fn dispatch(command: Command) -> ExitCode {
    let name = match command {
        Command::Setup(args) => {
            return match runwell_setup::run(args).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("runwell setup: {error}");
                    ExitCode::from(2)
                }
            };
        }
        Command::Version => {
            println!("runwell {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Controller => "controller",
        Command::Node => "node",
        Command::Report(args) => {
            return match runwell_report::execute(&args).await {
                Ok(text) => {
                    println!("{text}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("runwell report: {error}");
                    ExitCode::from(2)
                }
            };
        }
        Command::Simulate => "simulate",
        Command::Advise => "advise",
    };
    eprintln!("runwell {name}: not implemented in the M0 bootstrap (pre-alpha)");
    ExitCode::from(2)
}
