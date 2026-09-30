//! Command-line interface for humans and agents.
use crate::{
    Error, Report, Severity, context::Workflow, model::Finding, rules, timing::Trace, writer, yaml,
};
use clap::{Args, ValueEnum};
use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
};
/// Output format.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    /// Human-readable summary and rule tables.
    Md,
    /// Schema-versioned machine-readable JSON including applied diffs.
    Json,
}
/// Runner classification override.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum SelfHosted {
    /// Infer from runs-on labels; dynamic expressions remain unknown.
    #[default]
    Auto,
    /// Treat every job as self-hosted.
    Yes,
    /// Treat every job as GitHub-hosted.
    No,
}
/// Analyze GitHub Actions YAML without executing workflow code.
#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Exit codes: 0 = no warnings, 1 = warnings remain, 2 = invalid input or operation failed.\nAuto-fix rules: missing-concurrency, shared-home-cache, fixed-service-ports.\nFix output describes the analyzed snapshot; rerun after fixing to verify remaining findings."
)]
pub struct AdviseArgs {
    /// Directory containing .yml/.yaml workflows (a single file is also accepted).
    #[arg(long, default_value = ".github/workflows")]
    pub workflows: PathBuf,
    /// Optional JSONL baseline from runwell report --export-trace.
    #[arg(long)]
    pub trace: Option<PathBuf>,
    /// Markdown tables or schemaVersion: 1 JSON.
    #[arg(long, value_enum, default_value = "md")]
    pub format: Format,
    /// Apply provably local fixes atomically and print their unified diffs.
    #[arg(long)]
    pub fix: bool,
    /// Limit fixes to these stable rule IDs (repeatable; findings still include all rules).
    #[arg(long,requires="fix",value_parser=validate_rule)]
    pub rule: Vec<String>,
    /// Infer runner labels, or override inference for dynamic/larger runner labels.
    #[arg(long, value_enum, default_value = "auto")]
    pub self_hosted: SelfHosted,
}
fn validate_rule(value: &str) -> Result<String, String> {
    if rules::IDS.contains(&value) {
        Ok(value.into())
    } else {
        Err(format!(
            "unknown rule {value}; valid rules: {}",
            rules::IDS.join(", ")
        ))
    }
}
/// Analyze workflows and optionally apply safe edits. Returns rendered text and exit code.
pub fn execute(args: &AdviseArgs) -> Result<(String, u8), Error> {
    let trace = args
        .trace
        .as_ref()
        .map(|p| -> Result<_, Error> {
            Ok(Trace::new(runwell_trace::read_jsonl(BufReader::new(
                fs::File::open(p)?,
            ))?))
        })
        .transpose()?;
    let files = workflow_files(&args.workflows)?;
    let mut snapshots = Vec::new();
    let mut findings = Vec::new();
    // Parse the whole input before mutating any file.
    for path in files {
        let (source, stamp) = if args.fix {
            let (source, stamp) = writer::read(&path)?;
            (source, Some(stamp))
        } else {
            (fs::read_to_string(&path)?, None)
        };
        let name = path.to_string_lossy().into_owned();
        let mut found = analyze_source(&source, &name, args.self_hosted, trace.as_ref())
            .map_err(|e| Error::Parse(format!("{name}: {e}")))?;
        findings.append(&mut found);
        if let Some(stamp) = stamp {
            snapshots.push((path, source, stamp));
        }
    }
    let mut report = Report {
        schema_version: 1,
        findings,
        changes: vec![],
        refused: vec![],
    };
    if args.fix {
        let mut plans = Vec::new();
        for (path, source, stamp) in snapshots {
            let file = path.to_string_lossy();
            let mut edits = Vec::new();
            for f in report
                .findings
                .iter()
                .filter(|f| f.file == file && (args.rule.is_empty() || args.rule.contains(&f.rule)))
            {
                if let Some(e) = &f.edit {
                    edits.push(e.clone());
                } else {
                    report.refused.push(format!(
                        "{}:{} {}: {}",
                        f.file,
                        f.line,
                        f.rule,
                        f.fix
                            .reason
                            .as_deref()
                            .unwrap_or("manual fix requires review")
                    ));
                }
            }
            if edits.is_empty() {
                continue;
            }
            let changed = writer::apply(&source, edits)?;
            let change = writer::change(&file, &source, &changed);
            plans.push((path, source, changed, stamp, change));
        }
        for (path, source, _, stamp, _) in &plans {
            writer::verify(path, source.as_bytes(), stamp)?;
        }
        for (path, source, changed, stamp, change) in plans {
            if let Err(error) =
                writer::atomic_write_checked(&path, source.as_bytes(), changed.as_bytes(), &stamp)
            {
                let diffs = report
                    .changes
                    .iter()
                    .map(|c| c.diff.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(Error::Fix(format!("{error}\n{diffs}")));
            }
            report.changes.push(change);
        }
    }
    report.findings.sort_by(|a, b| {
        (&a.file, a.line, &a.rule, &a.job).cmp(&(&b.file, b.line, &b.rule, &b.job))
    });
    let code = u8::from(report.findings.iter().any(|f| f.severity != Severity::Info));
    let text = match args.format {
        Format::Json => serde_json::to_string_pretty(&report)?,
        Format::Md => crate::render::markdown(&report),
    };
    Ok((text, code))
}
pub(crate) fn analyze_source(
    source: &str,
    file: &str,
    mode: SelfHosted,
    trace: Option<&Trace>,
) -> Result<Vec<Finding>, Error> {
    let mut out = Vec::new();
    for root in yaml::parse(source)? {
        if !matches!(root.value, crate::yaml::Value::Map(_)) {
            return Err(Error::Parse("workflow document must be a mapping".into()));
        }
        if root
            .get("jobs")
            .is_none_or(|n| !matches!(n.value, crate::yaml::Value::Map(_)))
        {
            return Err(Error::Parse("workflow jobs must be a mapping".into()));
        }
        out.extend(rules::analyze(
            &Workflow {
                root: &root,
                source,
                file,
                mode,
            },
            trace,
        ));
    }
    Ok(out)
}
fn workflow_files(path: &Path) -> Result<Vec<PathBuf>, Error> {
    if path.is_file() {
        return Ok(vec![path.into()]);
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(path)? {
        let p = entry?.path();
        if p.is_file() && p.extension().is_some_and(|s| s == "yaml" || s == "yml") {
            files.push(p);
        }
    }
    files.sort();
    Ok(files)
}
