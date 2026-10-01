//! Single-binary entry point for runwell controller, node, and analysis commands.
//!
//! Controller and node execute trusted workloads; report/simulate/advise analyze CI.
//! Certificate issuance is offline and never prints private key material.

mod certs;
mod controller;
mod node;
#[cfg(target_os = "linux")]
mod node_network;
mod simulate;

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
    Controller(controller::Controller),
    /// Create an offline CA or issue/rotate peer certificates.
    Certs(certs::Certs),
    /// Run a Linux host execution node.
    Node(node::Node),
    /// Explain CI latency using GitHub workflow history.
    Report(Box<runwell_report::ReportArgs>),
    /// Replay a recorded job trace against scheduling policies.
    Simulate(simulate::Simulate),
    /// Suggest workflow improvements and agent-facing rules.
    Advise(runwell_advise::cli::AdviseArgs),
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
    match command {
        Command::Setup(args) => match runwell_setup::run(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runwell setup: {error}");
                ExitCode::from(2)
            }
        },
        Command::Version => {
            println!("runwell {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Controller(args) => match args.run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runwell controller: {error}");
                ExitCode::from(2)
            }
        },
        Command::Certs(args) => match args.run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runwell certs: {error}");
                ExitCode::from(2)
            }
        },
        Command::Node(args) => match args.run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runwell node: {error}");
                ExitCode::from(2)
            }
        },
        Command::Report(args) => match runwell_report::execute(&args).await {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("runwell report: {error}");
                ExitCode::from(2)
            }
        },
        Command::Simulate(args) => match args.run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runwell simulate: {error}");
                ExitCode::from(2)
            }
        },
        Command::Advise(args) => match runwell_advise::cli::execute(&args) {
            Ok((text, code)) => {
                println!("{text}");
                ExitCode::from(code)
            }
            Err(error) => {
                eprintln!("runwell advise: {error}");
                ExitCode::from(2)
            }
        },
    }
}
