//! Running a job's steps in a checkout.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;

use crate::workflow::{Job, DEFAULT_TIMEOUT_MINUTES};

/// The most output kept per step (the tail).
pub const MAX_STEP_OUTPUT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    Success,
    Failure,
    TimedOut,
}

impl Conclusion {
    /// The forge's check-run conclusion.
    pub fn as_str(self) -> &'static str {
        match self {
            Conclusion::Success => "success",
            Conclusion::Failure => "failure",
            Conclusion::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    Passed,
    Failed(Option<i32>),
    TimedOut,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct StepResult {
    pub name: String,
    pub outcome: StepOutcome,
    pub output: String,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct JobResult {
    pub conclusion: Conclusion,
    pub steps: Vec<StepResult>,
}

impl JobResult {
    /// A markdown summary for the check run.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for s in &self.steps {
            let mark = match &s.outcome {
                StepOutcome::Passed => "passed".to_string(),
                StepOutcome::Failed(Some(c)) => format!("failed (exit {c})"),
                StepOutcome::Failed(None) => "failed".to_string(),
                StepOutcome::TimedOut => "timed out".to_string(),
                StepOutcome::Skipped => "skipped".to_string(),
            };
            out.push_str(&format!("- {}: {mark} in {:.1}s\n", s.name, s.duration.as_secs_f64()));
        }
        if let Some(failed) = self.steps.iter().find(|s| matches!(s.outcome, StepOutcome::Failed(_) | StepOutcome::TimedOut)) {
            let tail: String = failed.output.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect();
            out.push_str(&format!("\nOutput of {}:\n```\n{tail}\n```\n", failed.name));
        }
        out
    }
}

fn keep_tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(MAX_STEP_OUTPUT);
    String::from_utf8_lossy(&bytes[start..]).to_string()
}

/// Run every step in order with a clean environment (`PATH`, `HOME` and the
/// declared variables only); the first failure skips the rest. The job's
/// timeout bounds the whole job.
pub async fn run_job(job: &Job, workdir: &Path, base_env: &BTreeMap<String, String>) -> JobResult {
    let minutes = job.timeout_minutes.unwrap_or(DEFAULT_TIMEOUT_MINUTES);
    run_job_with_deadline(job, workdir, base_env, Duration::from_secs(minutes * 60)).await
}

pub async fn run_job_with_deadline(job: &Job, workdir: &Path, base_env: &BTreeMap<String, String>, budget: Duration) -> JobResult {
    let started = Instant::now();
    let mut steps = Vec::new();
    let mut conclusion = Conclusion::Success;
    for step in &job.steps {
        let name = step.label();
        if conclusion != Conclusion::Success {
            steps.push(StepResult { name, outcome: StepOutcome::Skipped, output: String::new(), duration: Duration::ZERO });
            continue;
        }
        let remaining = budget.saturating_sub(started.elapsed());
        let mut env: BTreeMap<String, String> = ["PATH", "HOME"].iter().filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v))).collect();
        env.extend(base_env.clone());
        env.extend(job.env.clone());
        env.extend(step.env.clone());
        env.insert("CI".into(), "true".into());
        let t0 = Instant::now();
        let (outcome, output) = run_step(&step.run, workdir, &env, remaining).await;
        match outcome {
            StepOutcome::Passed => {}
            StepOutcome::TimedOut => conclusion = Conclusion::TimedOut,
            _ => conclusion = Conclusion::Failure,
        }
        steps.push(StepResult { name, outcome, output, duration: t0.elapsed() });
    }
    JobResult { conclusion, steps }
}

async fn run_step(script: &str, workdir: &Path, env: &BTreeMap<String, String>, budget: Duration) -> (StepOutcome, String) {
    if budget.is_zero() {
        return (StepOutcome::TimedOut, "job timeout reached before this step".into());
    }
    let spawned = tokio::process::Command::new("sh")
        .arg("-ec")
        .arg(script)
        .current_dir(workdir)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return (StepOutcome::Failed(None), format!("could not start the step: {e}")),
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let collect = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read_out = async {
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut out).await;
            }
        };
        let read_err = async {
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut err).await;
            }
        };
        tokio::join!(read_out, read_err);
        let status = child.wait().await;
        out.extend_from_slice(&err);
        (status, out)
    };
    match tokio::time::timeout(budget, collect).await {
        Ok((Ok(status), out)) if status.success() => (StepOutcome::Passed, keep_tail(&out)),
        Ok((Ok(status), out)) => (StepOutcome::Failed(status.code()), keep_tail(&out)),
        Ok((Err(e), out)) => (StepOutcome::Failed(None), format!("{}\nwaiting for the step failed: {e}", keep_tail(&out))),
        // Dropping the future drops the child, which kills it.
        Err(_) => (StepOutcome::TimedOut, "step exceeded the job timeout and was killed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{Job, Step};

    fn job(steps: &[&str]) -> Job {
        Job {
            name: None,
            timeout_minutes: None,
            env: BTreeMap::from([("GREETING".into(), "hello".into())]),
            steps: steps.iter().map(|s| Step { name: None, run: (*s).into(), env: BTreeMap::new() }).collect(),
        }
    }

    #[tokio::test]
    async fn steps_run_in_order_with_their_environment() {
        let dir = std::env::temp_dir().join(format!("rr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = run_job(&job(&["echo $GREETING > out.txt", "test \"$CI\" = true && cat out.txt"]), &dir, &BTreeMap::new()).await;
        assert_eq!(r.conclusion, Conclusion::Success);
        assert_eq!(r.steps[1].output.trim(), "hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failure_skips_the_rest_and_secrets_from_the_host_do_not_leak() {
        std::env::set_var("RITHUB_RUNNER_TEST_SECRET", "s3cret");
        let dir = std::env::temp_dir();
        let r = run_job(&job(&["echo \"[$RITHUB_RUNNER_TEST_SECRET]\"; exit 3", "echo never"]), &dir, &BTreeMap::new()).await;
        assert_eq!(r.conclusion, Conclusion::Failure);
        assert_eq!(r.steps[0].outcome, StepOutcome::Failed(Some(3)));
        assert_eq!(r.steps[0].output.trim(), "[]");
        assert_eq!(r.steps[1].outcome, StepOutcome::Skipped);
        assert!(r.summary().contains("failed (exit 3)"));
    }

    #[tokio::test]
    async fn the_job_timeout_kills_a_hung_step() {
        let r = run_job_with_deadline(&job(&["sleep 5", "echo after"]), &std::env::temp_dir(), &BTreeMap::new(), Duration::from_millis(200)).await;
        assert_eq!(r.conclusion, Conclusion::TimedOut);
        assert_eq!(r.steps[0].outcome, StepOutcome::TimedOut);
        assert_eq!(r.steps[1].outcome, StepOutcome::Skipped);
    }
}
