//! Versioned job-trace records shared by `runwell report` (producer) and
//! `runwell simulate` (consumer).
//!
//! A trace is JSON Lines: one [`TraceJob`] per line, one line per job attempt.
//! Invariants:
//! - `schema_version` is [`SCHEMA_VERSION`]; readers reject newer versions.
//! - New fields must be optional or `#[serde(default)]` so older traces still parse.
//! - Timestamps are UTC instants; a job that never started has no `started_at`.

use std::io::{BufRead, Write};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Current trace schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// One job attempt of one workflow run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceJob {
    pub schema_version: u32,
    /// `owner/name`.
    pub repo: String,
    pub workflow: Option<String>,
    pub run_id: u64,
    #[serde(default = "default_attempt")]
    pub run_attempt: u32,
    pub event: Option<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
    pub run_created_at: Option<Timestamp>,
    pub run_conclusion: Option<String>,
    #[serde(default)]
    pub job_id: Option<u64>,
    pub job_name: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub runner_name: Option<String>,
    pub created_at: Option<Timestamp>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    /// Job names this job depends on within the same run, when known.
    /// `None` means unknown (consumers may infer from timestamps).
    #[serde(default)]
    pub needs: Option<Vec<String>>,
    #[serde(default)]
    pub steps: Vec<TraceStep>,
}

/// One step of a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceStep {
    pub name: String,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub conclusion: Option<String>,
}

fn default_attempt() -> u32 {
    1
}

/// Errors reading or writing a trace.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("trace I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: invalid trace record: {source}")]
    Parse {
        line: usize,
        source: serde_json::Error,
    },
    #[error("line {line}: unsupported schema_version {found} (max {SCHEMA_VERSION})")]
    UnsupportedVersion { line: usize, found: u32 },
    #[error("failed to encode trace record: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Reads a JSONL trace, skipping blank lines.
pub fn read_jsonl<R: BufRead>(reader: R) -> Result<Vec<TraceJob>, TraceError> {
    let mut jobs = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let job: TraceJob = serde_json::from_str(&line).map_err(|source| TraceError::Parse {
            line: idx + 1,
            source,
        })?;
        if job.schema_version > SCHEMA_VERSION {
            return Err(TraceError::UnsupportedVersion {
                line: idx + 1,
                found: job.schema_version,
            });
        }
        jobs.push(job);
    }
    Ok(jobs)
}

/// Writes jobs as JSONL.
pub fn write_jsonl<W: Write>(mut writer: W, jobs: &[TraceJob]) -> Result<(), TraceError> {
    for job in jobs {
        serde_json::to_writer(&mut writer, job)?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"schema_version":1,"repo":"acme/app","workflow":"ci","run_id":1,"event":"pull_request","branch":"feat","run_created_at":"2026-01-01T00:00:00Z","run_conclusion":"success","job_name":"test","runner_name":"r1","created_at":"2026-01-01T00:00:01Z","started_at":"2026-01-01T00:00:05Z","completed_at":"2026-01-01T00:05:00Z","status":"completed","conclusion":"success","steps":[{"name":"Set up job","started_at":"2026-01-01T00:00:05Z","completed_at":"2026-01-01T00:00:06Z","conclusion":"success"}]}"#;

    #[test]
    fn round_trips_and_defaults_optional_fields() {
        let jobs = read_jsonl(format!("{LINE}\n\n").as_bytes()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].run_attempt, 1);
        assert_eq!(jobs[0].needs, None);
        let mut buf = Vec::new();
        write_jsonl(&mut buf, &jobs).unwrap();
        assert_eq!(read_jsonl(buf.as_slice()).unwrap(), jobs);
    }

    #[test]
    fn rejects_newer_schema() {
        let line = LINE.replace("\"schema_version\":1", "\"schema_version\":99");
        assert!(matches!(
            read_jsonl(line.as_bytes()),
            Err(TraceError::UnsupportedVersion { line: 1, found: 99 })
        ));
    }
}
