//! Versioned machine-readable advice.
use serde::Serialize;
use serde_json::Value;

/// Severity affects the command's exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational advice.
    Info,
    /// Actionable warning.
    Warn,
    /// Serious workflow problem.
    Error,
}
/// Estimated minutes saved on the affected path; estimates are not additive.
#[derive(Debug, Clone, Serialize)]
pub struct Savings {
    /// Median estimate.
    pub p50: f64,
    /// Tail estimate.
    pub p90: f64,
}
/// Safe edit or a manual suggestion.
#[derive(Debug, Clone, Serialize)]
pub struct Fix {
    /// `auto` or `manual`.
    pub kind: String,
    /// Suggested YAML or command.
    pub snippet: String,
    /// Why mechanical editing is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
/// One source-located actionable finding.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Stable rule identifier.
    pub rule: String,
    /// Diagnostic severity.
    pub severity: Severity,
    /// Workflow path.
    pub file: String,
    /// One-based source line.
    pub line: usize,
    /// YAML job key, or empty for workflow findings.
    pub job: String,
    /// Step name or ordinal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Explanation.
    pub message: String,
    /// Structured observations and source span.
    pub evidence: Value,
    /// Fix classification and suggestion.
    pub fix: Fix,
    /// Savings when supported by a trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_savings: Option<Savings>,
    #[serde(skip)]
    pub(crate) edit: Option<Edit>,
}
#[derive(Debug, Clone)]
pub(crate) struct Edit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}
/// Complete schema-versioned result, including diffs and refusals.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Output schema version.
    pub schema_version: u32,
    /// Deterministically ordered findings.
    pub findings: Vec<Finding>,
    /// Applied unified diffs (empty without --fix).
    pub changes: Vec<Change>,
    /// Requested edits that could not safely be applied.
    pub refused: Vec<String>,
}
/// An atomically applied workflow change.
#[derive(Debug, Serialize)]
pub struct Change {
    /// Workflow path.
    pub file: String,
    /// Unified diff of the changed bytes.
    pub diff: String,
}
