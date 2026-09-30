//! Workflow semantics and conservative command classification.
use crate::{cli::SelfHosted, yaml::Node};

pub(crate) struct Workflow<'a> {
    pub root: &'a Node,
    pub source: &'a str,
    pub file: &'a str,
    pub mode: SelfHosted,
}
pub(crate) struct Job<'a> {
    pub id: &'a str,
    pub node: &'a Node,
    pub workflow: &'a Workflow<'a>,
}
impl Workflow<'_> {
    pub fn triggered(&self, event: &str) -> bool {
        self.root.get("on").is_some_and(|n| {
            n.text() == event
                || n.items().iter().any(|v| v.text() == event)
                || n.get(event).is_some()
        })
    }
    pub fn jobs(&self) -> Vec<Job<'_>> {
        self.root.get("jobs").map_or(vec![], |n| {
            n.pairs()
                .iter()
                .filter(|(_, v)| v.get("uses").is_none())
                .map(|(k, v)| Job {
                    id: k.text(),
                    node: v,
                    workflow: self,
                })
                .collect()
        })
    }
}
impl Job<'_> {
    pub fn steps(&self) -> &[Node] {
        self.node.get("steps").map_or(&[], Node::items)
    }
    pub fn needs(&self) -> Vec<&str> {
        self.node.get("needs").map_or(vec![], Node::strings)
    }
    pub fn hosted(&self) -> bool {
        match self.workflow.mode {
            SelfHosted::Yes => true,
            SelfHosted::No => false,
            SelfHosted::Auto => self.node.get("runs-on").is_some_and(|n| {
                let labels = if let Some(labels) = n.get("labels") {
                    labels.strings()
                } else if n.get("group").is_some() {
                    return true;
                } else {
                    n.strings()
                };
                labels
                    .iter()
                    .any(|s| !s.contains("${{") && !github_image(s))
            }),
        }
    }
    pub fn gate(&self) -> bool {
        let cond = self.node.str("if");
        self.node.get("environment").is_some()
            || self.id.to_lowercase().contains("deploy")
            || self.id.to_lowercase().contains("release")
            || (cond.contains("github.ref")
                && (cond.contains("refs/heads/")
                    || cond.contains("default_branch")
                    || cond.contains("'main'")
                    || cond.contains("'master'")))
            || cond.contains("github.event.repository.default_branch")
    }
    pub fn env<'a>(&'a self, step: &'a Node, key: &str) -> Option<&'a Node> {
        step.get("env")
            .and_then(|n| n.get(key))
            .or_else(|| self.node.get("env").and_then(|n| n.get(key)))
            .or_else(|| self.workflow.root.get("env").and_then(|n| n.get(key)))
    }
}
fn github_image(label: &str) -> bool {
    const IMAGES: &[&str] = &[
        "ubuntu-latest",
        "ubuntu-24.04",
        "ubuntu-26.04",
        "ubuntu-26.04-arm",
        "ubuntu-22.04",
        "ubuntu-20.04",
        "ubuntu-24.04-arm",
        "ubuntu-22.04-arm",
        "ubuntu-slim",
        "windows-latest",
        "windows-2025",
        "windows-2025-vs2026",
        "windows-11-vs2026-arm",
        "windows-2022",
        "windows-2019",
        "windows-11-arm",
        "macos-latest",
        "xcode-27",
        "macos-26",
        "macos-26-intel",
        "macos-15",
        "macos-15-intel",
        "macos-14",
        "macos-13",
        "macos-12",
        "macos-14-large",
        "macos-14-xlarge",
        "macos-15-large",
        "macos-15-xlarge",
        "macos-26-large",
        "macos-26-xlarge",
    ];
    IMAGES.contains(&label)
}
pub(crate) fn commands(step: &Node) -> String {
    step.str("run")
        .lines()
        .filter(|l| {
            !["#", "echo ", "printf "]
                .iter()
                .any(|p| l.trim_start().starts_with(p))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub(crate) fn test_tool(step: &Node, job: &Job<'_>) -> Option<&'static str> {
    let s = commands(step).to_lowercase();
    if s.contains("--shard")
        || s.contains("--partition")
        || s.contains("--splits")
        || s.contains("-n auto")
        || job
            .node
            .get("strategy")
            .and_then(|n| n.get("matrix"))
            .is_some_and(|m| m.get("shard").is_some() || m.get("partition").is_some())
    {
        return None;
    }
    let tools = [
        ("go", vec!["go test", "gotestsum"]),
        ("pytest", vec!["pytest"]),
        ("jest", vec!["jest"]),
        ("vitest", vec!["vitest"]),
        ("nextest", vec!["nextest"]),
        ("cargo", vec!["cargo test"]),
        (
            "playwright",
            vec!["playwright test", "playwright:test", "playwright:smoke"],
        ),
        ("rspec", vec!["rspec"]),
        ("phpunit", vec!["phpunit"]),
    ];
    for (tool, needles) in tools {
        if needles.iter().any(|n| command_match(&s, n)) {
            if tool == "go" && !s.contains("./...") && !s.contains("gotestsum") {
                return None;
            }
            return Some(tool);
        }
    }
    if s.contains("make test")
        && job
            .steps()
            .iter()
            .any(|n| n.str("uses").contains("setup-go"))
    {
        return Some("go");
    }
    if [
        "bun run test",
        "bun run coverage",
        "scripts/run-coverage",
        "pnpm test",
        "pnpm run test",
        "npm test",
        "npm run test",
    ]
    .iter()
    .any(|n| s.contains(n))
    {
        return Some("script");
    }
    None
}
pub(crate) fn lint(step: &Node) -> bool {
    let s = format!("{} {}", commands(step), step.str("uses")).to_lowercase();
    [
        "golangci-lint",
        "eslint",
        "tsc --noemit",
        "ruff check",
        "cargo clippy",
        "biome check",
        "prettier --check",
        "run lint",
        "run typecheck",
    ]
    .iter()
    .any(|n| s.contains(n))
}
pub(crate) fn build(step: &Node) -> bool {
    let s = format!("{} {}", commands(step), step.str("uses"));
    [
        "docker build",
        "docker/build-push-action",
        "docker buildx build",
        "docker buildx bake",
        "docker/bake-action",
        ".github/actions/docker-build",
        "buildx build",
    ]
    .iter()
    .any(|n| s.contains(n))
}

fn command_match(script: &str, needle: &str) -> bool {
    script.match_indices(needle).any(|(i, _)| {
        let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.');
        !script[..i].chars().next_back().is_some_and(word)
            && !script[i + needle.len()..].chars().next().is_some_and(word)
    })
}
