use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Failure bucket; unknown is deliberately not retryable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Positive infrastructure evidence, with no test/code veto.
    Infra,
    /// Explicit test, lint, build or application failure.
    TestCode,
    /// Insufficient or conflicting evidence.
    Unknown,
}
/// Structured signals supplied by node/controller adapters, never inferred from
/// an arbitrary process nonzero exit alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    /// Kernel reported an OOM kill in the attributed job slice.
    OomKill,
    /// Runner process crashed (distinct from a workflow step exit).
    RunnerCrash,
    /// Assigned runner was lost.
    RunnerLost,
    /// Node carrying the job was lost.
    NodeLost,
    /// Docker daemon operation failed.
    DockerDaemonError,
    /// Timeout overlapped measured host PSI above its admission brake.
    TimeoutAbovePsiBrake,
    /// Assigned runner never picked up work before the pickup deadline.
    NeverPickedUp,
    /// Structured red-test, lint or compile result; unconditional veto.
    CodeFailure,
}
/// Input associated with one authoritative GitHub job attempt.
#[derive(Debug, Clone, Default)]
pub struct FailureEvidence {
    /// GitHub conclusion. Generic failure or timeout alone is insufficient.
    pub conclusion: String,
    /// Attributed runwell host/runner signals.
    pub signals: BTreeSet<Signal>,
    /// Check annotations plus a bounded log tail supplied by the integration.
    pub annotations_and_tail: String,
}
/// Classification and the rule ID responsible; contains no raw log content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// Decision bucket.
    pub class: FailureClass,
    /// Data-table rule ID, or a built-in guard ID.
    pub rule: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    rule: Vec<Rule>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: String,
    class: FailureClass,
    #[serde(default)]
    patterns: Vec<String>,
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(skip)]
    compiled: Option<regex::RegexSet>,
    #[serde(default)]
    signals: BTreeSet<Signal>,
}
/// Loaded once; case-insensitive literals and bounded, compiled diagnostic regexes.
pub struct Classifier {
    rules: Vec<Rule>,
    vetoes: Vec<Rule>,
    log_prefixes: regex::Regex,
}
/// Invalid rule data, without echoing possibly private custom patterns.
#[derive(Debug, thiserror::Error)]
#[error("invalid infrastructure classifier rule table")]
pub struct RuleError;
impl Classifier {
    /// Parse an operator rule table. Shipped test/code and ambiguity guards remain active
    /// even when the custom table omits or replaces them.
    pub fn from_toml(input: &str) -> Result<Self, RuleError> {
        let rules = parse(input)?;
        let vetoes = parse(crate::DEFAULT_RULES)?
            .into_iter()
            .filter(|r| r.class != FailureClass::Infra)
            .collect();
        // GitHub log timestamps and terminal colors are presentation, not part
        // of a framework's failure record. Strip them before anchored matching.
        let log_prefixes = regex::Regex::new(
            r"(?m)\x1b\[[0-?]*[ -/]*[@-~]|^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z[ \t]+",
        ).map_err(|_| RuleError)?;
        Ok(Self {
            rules,
            vetoes,
            log_prefixes,
        })
    }
    /// Load the shipped conservative table.
    pub fn builtin() -> Result<Self, RuleError> {
        Self::from_toml(crate::DEFAULT_RULES)
    }
    /// Code evidence wins even over OOM/shutdown evidence. A success, cancellation,
    /// neutral, or unknown conclusion can never become infra through a log match.
    pub fn classify(&self, evidence: &FailureEvidence) -> Classification {
        let text = self
            .log_prefixes
            .replace_all(&evidence.annotations_and_tail, "")
            .to_lowercase();
        let matched = |rule: &&Rule| {
            rule.signals.iter().any(|s| evidence.signals.contains(s))
                || rule.patterns.iter().any(|p| text.contains(p))
                || rule.compiled.as_ref().is_some_and(|r| r.is_match(&text))
        };
        if evidence.signals.contains(&Signal::CodeFailure) {
            return classification(FailureClass::TestCode, "code-failure");
        }
        if let Some(rule) = self
            .vetoes
            .iter()
            .filter(|r| r.class == FailureClass::TestCode)
            .chain(
                self.rules
                    .iter()
                    .filter(|r| r.class == FailureClass::TestCode),
            )
            .find(matched)
        {
            return classification(FailureClass::TestCode, &rule.id);
        }
        if !matches!(
            evidence.conclusion.as_str(),
            "failure" | "timed_out" | "startup_failure"
        ) {
            return classification(FailureClass::Unknown, "not-a-retryable-failure");
        }
        // Unattributed exceptions/network text cannot prove a host failure. Keep
        // these fail-closed guards even when an operator adds broader infra rules.
        if let Some(rule) = self
            .vetoes
            .iter()
            .chain(&self.rules)
            .filter(|r| r.class == FailureClass::Unknown)
            .find(matched)
        {
            return classification(FailureClass::Unknown, &rule.id);
        }
        if let Some(rule) = self
            .rules
            .iter()
            .filter(|r| r.class == FailureClass::Infra)
            .find(matched)
        {
            return classification(FailureClass::Infra, &rule.id);
        }
        classification(FailureClass::Unknown, "no-positive-evidence")
    }
}
fn classification(class: FailureClass, rule: &str) -> Classification {
    Classification {
        class,
        rule: rule.into(),
    }
}
fn parse(input: &str) -> Result<Vec<Rule>, RuleError> {
    let mut table: Table = toml::from_str(input).map_err(|_| RuleError)?;
    let mut ids = BTreeSet::new();
    for rule in &mut table.rule {
        if rule.id.is_empty()
            || !ids.insert(rule.id.clone())
            || (rule.signals.is_empty() && rule.patterns.is_empty() && rule.regexes.is_empty())
            || rule
                .patterns
                .iter()
                .chain(&rule.regexes)
                .any(|p| p.trim().is_empty())
        {
            return Err(RuleError);
        }
        for pattern in &mut rule.patterns {
            *pattern = pattern.to_lowercase();
        }
        if !rule.regexes.is_empty() {
            rule.compiled = Some(
                regex::RegexSetBuilder::new(&rule.regexes)
                    .case_insensitive(true)
                    .multi_line(true)
                    .size_limit(1_000_000)
                    .build()
                    .map_err(|_| RuleError)?,
            );
        }
    }
    Ok(table.rule)
}
