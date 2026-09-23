//! The indexable-document model. A single [`IndexDocument`] type covers every
//! searchable entity; its [`DocKind`] discriminates repos, issues, pull
//! requests, commits, and blobs. The `doc_id` is a stable, kind-prefixed key so
//! re-indexing the same entity **replaces** its prior document (upsert), never
//! duplicating it.

use serde::Serialize;

/// What an [`IndexDocument`] represents. Serialized as a lowercase string in
/// results so API consumers get a stable, self-describing `kind` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    Repo,
    Issue,
    PullRequest,
    Commit,
    Blob,
}

impl DocKind {
    /// The stable wire string for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Repo => "repo",
            DocKind::Issue => "issue",
            DocKind::PullRequest => "pull_request",
            DocKind::Commit => "commit",
            DocKind::Blob => "blob",
        }
    }

    /// Parse a wire string back into a kind (used when filtering a query).
    pub fn parse(s: &str) -> Option<DocKind> {
        match s {
            "repo" => Some(DocKind::Repo),
            "issue" => Some(DocKind::Issue),
            "pull_request" | "pull" | "pr" => Some(DocKind::PullRequest),
            "commit" => Some(DocKind::Commit),
            "blob" => Some(DocKind::Blob),
            _ => None,
        }
    }
}

/// One document to index. Construct via the kind-specific constructors so the
/// `doc_id` scheme stays consistent across the codebase.
#[derive(Clone, Debug)]
pub struct IndexDocument {
    /// Stable unique key (kind-prefixed). Re-indexing the same key upserts.
    pub doc_id: String,
    pub kind: DocKind,
    /// Owning repository id (as string) — the unit of permission scoping.
    pub repo_id: String,
    pub org: String,
    pub repo: String,
    /// `"public"` or `"private"` — mirrors the repository's visibility so a
    /// query can filter without a second lookup.
    pub visibility: String,
    /// Issue / PR number, or `0` when not applicable.
    pub number: i64,
    /// Commit or blob object id, or empty.
    pub oid: String,
    /// Blob path, or empty.
    pub path: String,
    /// Short, high-weight text (title / summary / repo name).
    pub title: String,
    /// Long, lower-weight text (body / commit message / blob content).
    pub body: String,
}

impl IndexDocument {
    /// Cap on how many bytes of a single field we keep. Repository blobs and
    /// long PR discussions are bounded so one giant document cannot dominate the
    /// index or a query's memory. Highlighting has its own, larger cap.
    pub const MAX_FIELD_BYTES: usize = 256 * 1024;

    fn clamp(mut s: String) -> String {
        if s.len() > Self::MAX_FIELD_BYTES {
            // Truncate on a char boundary at or below the cap.
            let mut end = Self::MAX_FIELD_BYTES;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
        s
    }

    /// A repository document (searchable by name + description).
    pub fn repo(
        repo_id: &str,
        org: &str,
        repo: &str,
        visibility: &str,
        description: &str,
    ) -> Self {
        Self {
            doc_id: format!("repo:{repo_id}"),
            kind: DocKind::Repo,
            repo_id: repo_id.to_string(),
            org: org.to_string(),
            repo: repo.to_string(),
            visibility: visibility.to_string(),
            number: 0,
            oid: String::new(),
            path: String::new(),
            title: repo.to_string(),
            body: Self::clamp(description.to_string()),
        }
    }

    /// An issue document.
    pub fn issue(
        repo_id: &str,
        org: &str,
        repo: &str,
        visibility: &str,
        number: i64,
        title: &str,
        body: &str,
    ) -> Self {
        Self {
            doc_id: format!("issue:{repo_id}:{number}"),
            kind: DocKind::Issue,
            repo_id: repo_id.to_string(),
            org: org.to_string(),
            repo: repo.to_string(),
            visibility: visibility.to_string(),
            number,
            oid: String::new(),
            path: String::new(),
            title: title.to_string(),
            body: Self::clamp(body.to_string()),
        }
    }

    /// A pull-request document.
    pub fn pull_request(
        repo_id: &str,
        org: &str,
        repo: &str,
        visibility: &str,
        number: i64,
        title: &str,
        body: &str,
    ) -> Self {
        Self {
            doc_id: format!("pull:{repo_id}:{number}"),
            kind: DocKind::PullRequest,
            repo_id: repo_id.to_string(),
            org: org.to_string(),
            repo: repo.to_string(),
            visibility: visibility.to_string(),
            number,
            oid: String::new(),
            path: String::new(),
            title: title.to_string(),
            body: Self::clamp(body.to_string()),
        }
    }

    /// A commit document (searchable by its message).
    pub fn commit(
        repo_id: &str,
        org: &str,
        repo: &str,
        visibility: &str,
        oid: &str,
        summary: &str,
        message: &str,
    ) -> Self {
        Self {
            doc_id: format!("commit:{repo_id}:{oid}"),
            kind: DocKind::Commit,
            repo_id: repo_id.to_string(),
            org: org.to_string(),
            repo: repo.to_string(),
            visibility: visibility.to_string(),
            number: 0,
            oid: oid.to_string(),
            path: String::new(),
            title: summary.to_string(),
            body: Self::clamp(message.to_string()),
        }
    }

    /// A blob document (searchable by path + content). `oid` records the blob
    /// object id at index time for provenance.
    pub fn blob(
        repo_id: &str,
        org: &str,
        repo: &str,
        visibility: &str,
        path: &str,
        oid: &str,
        content: &str,
    ) -> Self {
        Self {
            doc_id: format!("blob:{repo_id}:{path}"),
            kind: DocKind::Blob,
            repo_id: repo_id.to_string(),
            org: org.to_string(),
            repo: repo.to_string(),
            visibility: visibility.to_string(),
            number: 0,
            oid: oid.to_string(),
            path: path.to_string(),
            title: path.to_string(),
            body: Self::clamp(content.to_string()),
        }
    }
}
