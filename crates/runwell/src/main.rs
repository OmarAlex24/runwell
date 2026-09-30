//! Single-binary entry point for runwell controller, node, and analysis commands.
//!
//! M0 exposes the CLI contract only: unfinished commands report their status and
//! exit with code 2 without starting a runner, opening sockets, or modifying state.

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
    Report,
    /// Replay a recorded job trace against scheduling policies.
    Simulate,
    /// Suggest workflow improvements and agent-facing rules.
    Advise,
    /// Print the runwell version.
    Version,
}

fn main() -> ExitCode {
    dispatch(Cli::parse().command)
}

fn dispatch(command: Command) -> ExitCode {
    let name = match command {
        Command::Version => {
            println!("runwell {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Controller => "controller",
        Command::Node => "node",
        Command::Report => "report",
        Command::Simulate => "simulate",
        Command::Advise => "advise",
    };
    eprintln!("runwell {name}: not implemented in the M0 bootstrap (pre-alpha)");
    ExitCode::from(2)
}
