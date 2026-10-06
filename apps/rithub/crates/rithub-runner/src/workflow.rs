//! Workflow files: `.rithub/workflows/*.yml` (or `.yaml`).
//!
//! ```yaml
//! name: CI
//! on: [push, pull_request]
//! env: { RUST_LOG: info }
//! jobs:
//!   test:
//!     name: Unit tests
//!     timeout_minutes: 10
//!     steps:
//!       - name: Build
//!         run: cargo build
//!       - run: cargo test
//!         env: { CI: "true" }
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// Where workflows live in a checkout.
pub const WORKFLOW_DIR: &str = ".rithub/workflows";
/// Default and maximum job timeouts, in minutes.
pub const DEFAULT_TIMEOUT_MINUTES: u64 = 30;
pub const MAX_TIMEOUT_MINUTES: u64 = 360;

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("{file}: {message}")]
    Invalid { file: String, message: String },
    #[error("reading {file}: {message}")]
    Io { file: String, message: String },
}

/// The events a workflow runs on: a single name or a list.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum On {
    One(String),
    Many(Vec<String>),
}

impl On {
    pub fn matches(&self, event: &str) -> bool {
        match self {
            On::One(e) => e == event,
            On::Many(es) => es.iter().any(|e| e == event),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Step {
    #[serde(default)]
    pub name: Option<String>,
    pub run: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Step {
    /// The step's display name (its first command line when unnamed).
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.run.lines().next().unwrap_or("step").trim().to_string())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Job {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub timeout_minutes: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub on: On,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub jobs: BTreeMap<String, Job>,
}

impl Workflow {
    /// Parse and validate one workflow file's text.
    pub fn parse(file: &str, text: &str) -> Result<Self, WorkflowError> {
        let invalid = |message: String| WorkflowError::Invalid { file: file.to_string(), message };
        let wf: Workflow = serde_yaml::from_str(text).map_err(|e| invalid(e.to_string()))?;
        if wf.name.trim().is_empty() {
            return Err(invalid("name is required".into()));
        }
        if wf.jobs.is_empty() {
            return Err(invalid("at least one job is required".into()));
        }
        for (id, job) in &wf.jobs {
            if job.steps.is_empty() {
                return Err(invalid(format!("job {id} has no steps")));
            }
            if job.steps.iter().any(|s| s.run.trim().is_empty()) {
                return Err(invalid(format!("job {id} has a step with an empty run")));
            }
            if job.timeout_minutes.is_some_and(|t| t == 0 || t > MAX_TIMEOUT_MINUTES) {
                return Err(invalid(format!("job {id}: timeout_minutes must be 1..={MAX_TIMEOUT_MINUTES}")));
            }
        }
        Ok(wf)
    }
}

/// Every workflow in a checkout, sorted by file name.
pub fn load_dir(checkout: &Path) -> Result<Vec<(String, Workflow)>, WorkflowError> {
    let dir = checkout.join(WORKFLOW_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let file = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let text = std::fs::read_to_string(&p).map_err(|e| WorkflowError::Io { file: file.clone(), message: e.to_string() })?;
            Workflow::parse(&file, &text).map(|w| (file, w))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CI: &str = "name: CI\non: [push, pull_request]\nenv: { A: '1' }\njobs:\n  test:\n    timeout_minutes: 5\n    steps:\n      - name: Hello\n        run: echo hi\n      - run: |\n          echo two\n          echo lines\n";

    #[test]
    fn parses_events_jobs_and_steps() {
        let wf = Workflow::parse("ci.yml", CI).unwrap();
        assert!(wf.on.matches("push") && wf.on.matches("pull_request") && !wf.on.matches("release"));
        let job = &wf.jobs["test"];
        assert_eq!(job.timeout_minutes, Some(5));
        assert_eq!(job.steps[0].label(), "Hello");
        assert_eq!(job.steps[1].label(), "echo two");
        assert_eq!(wf.env["A"], "1");
        assert!(Workflow::parse("x.yml", "name: X\non: push\njobs:\n  a:\n    steps:\n      - run: ls\n").unwrap().on.matches("push"));
    }

    #[test]
    fn rejects_invalid_workflows() {
        assert!(Workflow::parse("a.yml", "name: X\non: push\njobs: {}\n").is_err());
        assert!(Workflow::parse("a.yml", "name: X\non: push\njobs:\n  a:\n    steps: []\n").is_err());
        assert!(Workflow::parse("a.yml", "name: X\non: push\njobs:\n  a:\n    timeout_minutes: 0\n    steps:\n      - run: ls\n").is_err());
        assert!(Workflow::parse("a.yml", "not: [valid").is_err());
    }
}
