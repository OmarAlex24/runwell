use clap::{Args, ValueEnum};
use runwell_sim::{Config, Policy};
use std::{
    fs::File,
    io::{BufReader, Write},
    path::PathBuf,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    Md,
    Json,
}

#[derive(Debug, Args)]
pub struct Simulate {
    /// runwell-trace JSONL input.
    #[arg(long)]
    trace: PathBuf,
    /// TOML hosts, resource demands and baseline assumptions.
    #[arg(long)]
    hosts: PathBuf,
    /// all, baseline, runwell-equivalent, fifo, shortest, critical-path, fair-share, or runwell.
    #[arg(long, default_value = "all")]
    policy: String,
    #[arg(long, value_enum, default_value = "md")]
    format: Format,
    /// Override the seeded failure model.
    #[arg(long)]
    seed: Option<u64>,
    /// Import a workflow graph; repeat for each repository: REPO=PATH.
    #[arg(long, value_name = "REPO=PATH")]
    workflow_needs: Vec<String>,
    /// Earliest run creation using that graph: REPO=RFC3339.
    #[arg(long, value_name = "REPO=RFC3339")]
    workflow_since: Vec<String>,
    /// Compare resource policies at 1, 1.25, 1.5, 2, 2.5 and 3 times capacity.
    #[arg(long)]
    overcommit_sweep: bool,
    /// Exhaustively search persistent runner allocations on the configured hosts.
    #[arg(long)]
    classic_search: bool,
    /// Per-host total runner search bounds; defaults to physical core counts.
    #[arg(long, value_delimiter = ',')]
    classic_max_runners: Vec<usize>,
    /// Repository-ordered p90 targets in minutes; defaults to observed p90 / 2.
    #[arg(long, value_delimiter = ',')]
    target_p90: Vec<f64>,
}

impl Simulate {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let mut jobs = runwell_trace::read_jsonl(BufReader::new(File::open(self.trace)?))?;
        let mut config: Config = toml::from_str(&std::fs::read_to_string(self.hosts)?)?;
        if let Some(seed) = self.seed {
            config.seed = seed;
        }
        for binding in &self.workflow_since {
            let (repo, _) = binding
                .split_once('=')
                .ok_or("workflow-since must be REPO=RFC3339")?;
            if !self
                .workflow_needs
                .iter()
                .any(|b| b.split_once('=').is_some_and(|(r, _)| r == repo))
            {
                return Err("workflow-since needs a matching workflow-needs repository".into());
            }
        }
        for binding in &self.workflow_needs {
            let (repo, path) = binding
                .split_once('=')
                .ok_or("workflow-needs must be REPO=PATH")?;
            let since = self
                .workflow_since
                .iter()
                .filter_map(|b| b.split_once('='))
                .find(|(r, _)| *r == repo)
                .map(|(_, t)| t.parse())
                .transpose()?;
            let workflow = runwell_sim::workflow::WorkflowNeeds::parse(
                repo.into(),
                &std::fs::read_to_string(path)?,
                since,
            )?;
            config
                .report_workflows
                .push(runwell_sim::config::WorkflowScope {
                    repo: repo.into(),
                    name: workflow.name().into(),
                });
            let summary = workflow.apply(&mut jobs)?;
            eprintln!(
                "workflow import: {} graph runs, {} inferred runs, {} cancellation groups",
                summary.graph_runs, summary.inferred_runs, summary.cancellation_runs
            );
        }
        let policies = if self.policy == "all" {
            Policy::ALL.to_vec()
        } else {
            vec![Policy::parse(&self.policy)?]
        };
        if self.overcommit_sweep {
            config.overcommit_sweep = runwell_sim::experiments::OVERCOMMIT_SWEEP.to_vec();
        }
        let prepared = runwell_sim::PreparedTrace::new(&jobs, &config)?;
        let mut report = runwell_sim::compare(&prepared, &policies)?;
        if self.classic_search {
            eprintln!("Searching bounded persistent-runner allocations...");
            report.classic = Some(runwell_sim::search::search_allocations(
                &prepared,
                &runwell_sim::search::SearchOptions {
                    max_runners_per_host: self.classic_max_runners,
                    target_p90_minutes: self.target_p90,
                    ..Default::default()
                },
            )?);
        }
        let mut stdout = std::io::stdout().lock();
        match self.format {
            Format::Md => stdout.write_all(report.markdown().as_bytes())?,
            Format::Json => {
                serde_json::to_writer_pretty(&mut stdout, &report)?;
                writeln!(stdout)?;
            }
        }
        Ok(())
    }
}
