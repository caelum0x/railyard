//! # rithub-runner
//!
//! The workflow executor behind rithub's external-runner contract: given a
//! checkout at a commit, it loads `.rithub/workflows/*.yml`, runs every job of
//! every workflow that listens to the event, and reports one check run per job
//! (`{workflow} / {job}`) through the forge's runner API: `in_progress` when
//! it starts, `completed` with `success`, `failure` or `timed_out` and a
//! step-by-step summary when it ends.

pub mod exec;
pub mod report;
pub mod workflow;

use std::path::Path;

pub use exec::{Conclusion, JobResult};
pub use report::{HttpReporter, Reporter};
pub use workflow::{Workflow, WorkflowError};

/// What one job did.
#[derive(Debug, Clone)]
pub struct JobReport {
    pub check_name: String,
    pub check_run_id: String,
    pub conclusion: Conclusion,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    #[error(transparent)]
    Report(#[from] report::ReportError),
}

/// Run the workflows of `checkout` that listen to `event` for `head_sha`.
/// A broken workflow file is reported as a failed check run of its own, so
/// the author sees it on the commit.
pub async fn run_commit(checkout: &Path, head_sha: &str, event: &str, reporter: &dyn Reporter) -> Result<Vec<JobReport>, RunError> {
    let workflows = match workflow::load_dir(checkout) {
        Ok(w) => w,
        Err(e) => {
            let id = reporter.start("workflows", head_sha).await?;
            reporter.finish(&id, "failure", "Invalid workflow", &e.to_string()).await?;
            return Ok(vec![JobReport { check_name: "workflows".into(), check_run_id: id, conclusion: Conclusion::Failure }]);
        }
    };
    let mut reports = Vec::new();
    for (_, wf) in workflows.iter().filter(|(_, wf)| wf.on.matches(event)) {
        for (job_id, job) in &wf.jobs {
            let check_name = format!("{} / {}", wf.name, job.name.clone().unwrap_or_else(|| job_id.clone()));
            let id = reporter.start(&check_name, head_sha).await?;
            let result = exec::run_job(job, checkout, &wf.env).await;
            let title = format!("{} {}", check_name, result.conclusion.as_str());
            reporter.finish(&id, result.conclusion.as_str(), &title, &result.summary()).await?;
            reports.push(JobReport { check_name, check_run_id: id, conclusion: result.conclusion });
        }
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    #[async_trait]
    impl Reporter for Recorder {
        async fn start(&self, name: &str, sha: &str) -> Result<String, report::ReportError> {
            let mut log = self.0.lock().unwrap();
            log.push(format!("start {name} {sha}"));
            Ok(format!("id{}", log.len()))
        }
        async fn finish(&self, id: &str, conclusion: &str, _title: &str, _summary: &str) -> Result<(), report::ReportError> {
            self.0.lock().unwrap().push(format!("finish {id} {conclusion}"));
            Ok(())
        }
    }

    fn checkout(files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(workflow::WORKFLOW_DIR)).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(workflow::WORKFLOW_DIR).join(name), text).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn runs_matching_workflows_and_reports_each_job() {
        let dir = checkout(&[
            ("ci.yml", "name: CI\non: push\njobs:\n  ok:\n    steps:\n      - run: 'true'\n  bad:\n    steps:\n      - run: 'false'\n"),
            ("release.yml", "name: Release\non: release\njobs:\n  r:\n    steps:\n      - run: 'true'\n"),
        ]);
        let rec = Recorder::default();
        let reports = run_commit(&dir, "abc", "push", &rec).await.unwrap();
        let got: Vec<(String, &str)> = reports.iter().map(|r| (r.check_name.clone(), r.conclusion.as_str())).collect();
        assert_eq!(got, vec![("CI / bad".into(), "failure"), ("CI / ok".into(), "success")]);
        assert_eq!(rec.0.lock().unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_broken_workflow_becomes_a_failed_check() {
        let dir = checkout(&[("ci.yml", "name: CI\non: push\njobs: {}\n")]);
        let rec = Recorder::default();
        let reports = run_commit(&dir, "abc", "push", &rec).await.unwrap();
        assert_eq!((reports[0].check_name.as_str(), reports[0].conclusion), ("workflows", Conclusion::Failure));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
