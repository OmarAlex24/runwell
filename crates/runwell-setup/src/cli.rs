//! clap contract shared by the runwell binary and automated callers.
use crate::{
    Error,
    model::{HostTarget, ProbedHost, Recommender, RepoTarget, StubRecommender},
    probe, session,
    ssh::SshClient,
    warnings, wizard,
};
use clap::{Args, Subcommand};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

#[derive(Debug, Args)]
#[command(
    about = "Discover Linux hosts over SSH and resume a local CI setup session",
    long_about = "Discover Linux hosts using batch SSH key authentication. The remote probe is read-only. Recommendation and installation are reserved for later milestones. With no options, an interactive wizard resumes the local session. --host/--repo or --json run without prompts.",
    subcommand_negates_reqs = true
)]
pub struct SetupArgs {
    #[command(subcommand)]
    pub command: Option<SetupCommand>,
    /// SSH destination user@host[:port]; repeat for multiple hosts.
    #[arg(long, value_name = "USER@HOST[:PORT]")]
    pub host: Vec<HostTarget>,
    /// Repository owner/name; repeat for multiple repositories.
    #[arg(long, value_name = "OWNER/REPO")]
    pub repo: Vec<RepoTarget>,
    /// Print the session as JSON without interactive prompts.
    #[arg(long)]
    pub json: bool,
    /// Local resume file (default: platform config directory/runwell/setup-session.json).
    #[arg(long, value_name = "PATH")]
    pub state_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum SetupCommand {
    /// Print read-only facts for one host; does not save a setup session.
    #[command(
        long_about = "Probe one Linux host using the system ssh binary and key authentication only. Uses BatchMode=yes, ConnectTimeout=10, and StrictHostKeyChecking=accept-new. No remote files are written. Missing tools or privileges produce explicit unknown reasons. Runner release ages are checked locally against public GitHub metadata and remain unknown offline. --json prints only the host facts; this command never prompts or saves wizard state."
    )]
    Probe(ProbeArgs),
}

#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// SSH destination; bracket IPv6 literals, for example user@[::1]:2222.
    #[arg(long, value_name = "USER@HOST[:PORT]")]
    pub host: HostTarget,
    /// Emit a single HostFacts JSON document on stdout.
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: SetupArgs) -> Result<(), Error> {
    let client = SshClient::default();
    if let Some(SetupCommand::Probe(args)) = args.command {
        let facts = probe::collect(&client, &args.host).await?;
        if args.json {
            print_warnings(&facts);
            write_json(&facts)?;
        } else {
            print_facts(&args.host, &facts);
        }
        return Ok(());
    }
    let interactive = !args.json && args.host.is_empty() && args.repo.is_empty();
    if interactive && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
        return Err(Error::Usage("interactive setup requires a terminal; supply --host user@host --repo owner/repo --json".into()));
    }
    let path = match args.state_file {
        Some(path) => path,
        None => session::state_path()?,
    };
    let mut state = session::load(&path)?;
    let (hosts, repos) = if interactive {
        wizard::targets(&state)?
    } else {
        (args.host, args.repo)
    };
    if state.hosts.is_empty() && hosts.is_empty() {
        return Err(Error::Usage(
            "supply at least one --host user@host, or resume a session containing hosts".into(),
        ));
    }
    for repo in repos {
        if !state.repos.contains(&repo) {
            state.repos.push(repo);
        }
    }
    // Recomputed recommendations must never refer to facts from a previous run.
    state.recommendation = None;
    state.plan = None;
    session::save(&path, &state)?;
    for target in hosts {
        let facts = if interactive {
            wizard::authenticate(&client, &target).await?;
            let mut facts = probe::parse_output(&client.probe(&target).await?)?;
            crate::releases::enrich(&mut facts).await;
            facts
        } else {
            probe::collect(&client, &target).await?
        };
        let host = ProbedHost { target, facts };
        if let Some(existing) = state.hosts.iter_mut().find(|h| h.target == host.target) {
            *existing = host;
        } else {
            state.hosts.push(host);
        }
        // Progress survives an interruption or an authentication failure on the next host.
        session::save(&path, &state)?;
    }
    match StubRecommender.recommend(&state.hosts, &[]) {
        Ok(recommendation) => state.recommendation = Some(recommendation),
        Err(Error::Unimplemented) => eprintln!(
            "Discovery complete. Recommendation is unimplemented in this milestone; no architecture was selected."
        ),
        Err(error) => return Err(error),
    }
    session::save(&path, &state)?;
    if args.json {
        for host in &state.hosts {
            print_warnings(&host.facts);
        }
        write_json(&state)?;
    } else {
        for host in &state.hosts {
            print_facts(&host.target, &host.facts);
        }
        println!("Session saved to {}", path.display());
    }
    Ok(())
}

fn write_json(value: &impl serde::Serialize) -> Result<(), Error> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)?;
    Ok(())
}

fn print_warnings(facts: &crate::HostFacts) {
    for warning in warnings::derive(facts, &[]) {
        eprintln!("Warning [{}]: {}", warning.code, warning.message);
    }
}

fn print_facts(target: &HostTarget, facts: &crate::HostFacts) {
    println!("Host {target}");
    for (name, fact) in [
        ("OS", &facts.os),
        ("Kernel", &facts.kernel),
        ("Architecture", &facts.arch),
    ] {
        println!(
            "  {name}: {}",
            fact.value
                .as_deref()
                .unwrap_or_else(|| fact.unknown_reason.as_deref().unwrap_or("unknown"))
        );
    }
    println!(
        "  Privileged probe items: {}",
        match facts.sudo_available.value {
            Some(true) => "available (root or sudo -n)",
            Some(false) => "limited (no noninteractive sudo)",
            None => "unknown",
        }
    );
    for warning in warnings::derive(facts, &[]) {
        println!("  Warning [{}]: {}", warning.code, warning.message);
    }
    println!("  Use --json to inspect all observations and unknown reasons.");
}
