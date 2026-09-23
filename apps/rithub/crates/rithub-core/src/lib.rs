//! # rithub-core
//!
//! The forge **domain**: the pure data model (`User`, `Organization`,
//! `Repository`, `Membership`, and the `Role` permission scale) plus a
//! repository-pattern [`Store`] trait and its Postgres implementation.
//!
//! This crate owns **no** git internals and **no** HTTP concerns — it is the
//! metadata plane. The object/ref plane lives in `rit` (behind `rithub-git`),
//! and the transport lives in `rithub-server`. Keeping the store behind a trait
//! keeps persistence swappable and the domain testable.

pub mod access;
pub mod audit;
pub mod ci;
pub mod deploykeys;
pub mod domain;
pub mod error;
pub mod issues;
pub mod jobs;
pub mod milestones;
pub mod notifications;
pub mod numbering;
pub mod pulls;
pub mod releases;
pub mod reviews;
pub mod search;
pub mod sshkeys;
pub mod statuses;
pub mod store;
pub mod token;
pub mod validate;
pub mod webhooks;

pub use access::{AccessStore, RepoRole, Team};
pub use audit::{AuditEvent, AuditStore, NewAuditEvent};
pub use ci::{
    CheckConclusion, CheckRun, CheckRunPatch, CheckRunStatus, CiStore, NewCheckRun, Runner,
};
pub use deploykeys::DeployKeyStore;
pub use domain::{
    DeployKey, Membership, NewOrganization, NewRepository, Organization, Repository, Role, Token,
    UpdateRepository, User, Visibility,
};
pub use error::{CoreError, Result};
pub use issues::{Issue, IssueComment, IssueState, IssueStore, Label, NewIssue, UpdateIssue};
pub use milestones::{
    Milestone, MilestoneStore, MilestoneWithProgress, NewMilestone, UpdateMilestone,
};
pub use notifications::{Notification, NotifyStore};
pub use pulls::{NewPullRequest, PrComment, PrState, PullRequest, PullRequestStore, UpdatePr};
pub use releases::{NewRelease, Release, ReleaseAsset, ReleaseAssetStore, ReleaseStore};
pub use reviews::{
    BranchProtection, NewReviewComment, Review, ReviewComment, ReviewState, ReviewStore,
};
pub use search::{SearchHit, SearchStore};
pub use sshkeys::SshKeyStore;
pub use statuses::{CheckState, CommitStatus, NewStatus, StatusStore};
pub use store::{PgStore, Store};
pub use token::{generate_deploy_token, generate_runner_token, generate_token, hash_token};
pub use validate::validate_name;
pub use jobs::{
    OutboxJob, OutboxStore, KIND_AUDIT_APPEND, KIND_CHECK_RUN_DISPATCH, KIND_EMAIL_NOTIFICATION,
    KIND_NOTIFICATION_FAN_OUT, KIND_WEBHOOK_DELIVERY, MAX_ATTEMPTS,
};
pub use webhooks::{NewWebhook, Webhook, WebhookStore};
