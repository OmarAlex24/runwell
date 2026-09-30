//! Persistent runners sharing HOME can contend on tool locks/caches. Detect cache
//! consumers without an isolated effective workflow/job/step env or explicit
//! command override; hosted jobs are excluded. Preserve existing cache choices.
//! Only insert job env locally; aliases/flow maps and shared fixed paths are manual.
use super::*;
use crate::{context::commands, local};
const CACHE_ROOT: &str = "${{ github.workspace }}/../.runwell-cache/${{ github.run_id }}-${{ github.run_attempt }}-${{ github.job }}-${{ strategy.job-index || 0 }}";
pub(super) fn check(job: &Job<'_>, out: &mut Vec<Finding>) {
    if !job.hosted() {
        return;
    }
    let tools = [
        (
            "golangci-lint",
            "GOLANGCI_LINT_CACHE",
            "golangci-lint",
            "--cache-dir",
        ),
        ("go", "GOCACHE", "go-build", "GOCACHE="),
        ("go", "GOMODCACHE", "go-mod", "GOMODCACHE="),
        ("npm", "npm_config_cache", "npm", "--cache"),
        ("pnpm", "pnpm_config_store_dir", "pnpm", "--store-dir"),
        ("pip", "PIP_CACHE_DIR", "pip", "--cache-dir"),
        ("cargo", "CARGO_HOME", "cargo", "CARGO_HOME="),
        (
            "playwright",
            "PLAYWRIGHT_BROWSERS_PATH",
            "playwright",
            "PLAYWRIGHT_BROWSERS_PATH=",
        ),
    ];
    let mut missing = Vec::new();
    let mut fixed = Vec::new();
    for (tool, key, dir, flag) in tools {
        let users: Vec<_> = job
            .steps()
            .iter()
            .filter(|s| {
                let script = commands(s);
                let uses = s.str("uses");
                script
                    .split(|c: char| !c.is_alphanumeric() && c != '-')
                    .any(|t| t == tool)
                    || uses.split(['/', '@']).any(|part| {
                        part == tool
                            || part == format!("setup-{tool}")
                            || part == format!("{tool}-action")
                    })
                    || (tool == "go" && script.contains("make test"))
            })
            .collect();
        if users.is_empty() {
            continue;
        }
        let unisolated = users.iter().any(|s| {
            let script = commands(s);
            if (script.contains(flag) && isolated(&script))
                || (tool == "pnpm"
                    && script.contains("pnpm config set store-dir")
                    && isolated(&script))
                || (tool == "pip" && script.contains("--no-cache-dir"))
                || job.env(s, "HOME").is_some_and(|n| isolated(n.text()))
            {
                return false;
            }
            let effective = job.env(s, key).or_else(|| {
                if tool == "pnpm" && legacy_pnpm(job) {
                    job.env(s, "npm_config_store_dir")
                } else {
                    None
                }
            });
            effective.is_none_or(|v| !isolated(v.text()))
        });
        if !unisolated {
            continue;
        }
        if (tool == "pnpm"
            && users
                .iter()
                .any(|s| job.env(s, "npm_config_store_dir").is_some()))
            || users.iter().any(|s| commands(s).contains(flag))
            || job.node.get("env").and_then(|n| n.get(key)).is_some()
            || job
                .workflow
                .root
                .get("env")
                .and_then(|n| n.get(key))
                .is_some()
            || users
                .iter()
                .any(|s| s.get("env").and_then(|n| n.get(key)).is_some())
        {
            fixed.push(key);
        } else {
            missing.push((key, dir));
        }
    }
    if missing.is_empty() && fixed.is_empty() {
        return;
    }
    let suggested: Vec<_> = missing
        .iter()
        .copied()
        .chain(fixed.iter().filter_map(|key| {
            tools
                .iter()
                .find(|(_, k, _, _)| k == key)
                .map(|(_, k, dir, _)| (*k, *dir))
        }))
        .collect();
    let mut entries: Vec<String> = suggested
        .iter()
        .map(|(key, dir)| format!("{key}: {CACHE_ROOT}/{dir}"))
        .collect();
    if suggested.iter().any(|(k, _)| *k == "pnpm_config_store_dir") {
        entries.push(format!("npm_config_store_dir: {CACHE_ROOT}/pnpm"));
    }
    let snippet = format!(
        "env:\n{}",
        entries
            .iter()
            .map(|s| format!("  {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut f = job_finding(
        job,
        job.node,
        "shared-home-cache",
        "Cache consumers have no provable job/runner isolation in this workflow. If runners share HOME, tool locks can contend; verify composite actions and external tool configuration before isolating caches.",
        &snippet,
        json!({"missingCacheEnvironment":missing.iter().map(|(k,_)| k).collect::<Vec<_>>(),"sharedCacheEnvironment":fixed,"tradeoff":"run/job/matrix isolation may lose warm cache reuse; manage retention or choose an existing unique persistent per-runner cache"}),
    );
    let edit = if !fixed.is_empty() {
        Err("existing cache paths require review; they will not be overwritten".into())
    } else if job.node.inherited()
        || job.workflow.root.inherited()
        || job.workflow.root.get("jobs").is_some_and(|n| n.inherited())
    {
        Err("job or workflow is shared through anchors or merge keys".into())
    } else if let Some(env) = job.node.get("env") {
        local::insert(job.workflow.source, env, &entries)
    } else {
        let pad = " ".repeat(local::unit(job.node));
        let mut lines = vec!["env:".into()];
        lines.extend(entries.iter().map(|s| format!("{pad}{s}")));
        local::insert(job.workflow.source, job.node, &lines)
    };
    auto(&mut f, edit);
    out.push(f);
}
fn isolated(s: &str) -> bool {
    [
        "runner.temp",
        "runner.name",
        "github.run_id",
        "RUNNER_TEMP",
        "RUNNER_NAME",
    ]
    .iter()
    .any(|n| s.contains(n))
}

fn legacy_pnpm(job: &Job<'_>) -> bool {
    job.steps()
        .iter()
        .filter(|s| s.str("uses").contains("pnpm/action-setup"))
        .filter_map(|s| s.get("with").map(|w| w.str("version")))
        .any(|v| {
            v.split('.')
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n <= 10)
        })
}
