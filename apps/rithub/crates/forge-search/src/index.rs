//! The Tantivy-backed full-text index.
//!
//! ## Schema
//!
//! Each document carries a `doc_id` (unique key), text fields `title` (boosted)
//! and `body`, and a set of stored/indexed string fields used for scoping and
//! result rendering: `kind`, `repo_id`, `org`, `repo`, `visibility`, `number`,
//! `oid`, `path`.
//!
//! ## Upsert semantics
//!
//! Tantivy has no in-place update, so [`SearchIndex::upsert`] deletes any prior
//! document with the same `doc_id` term and adds the new one. Writes are
//! serialized behind a mutex; a commit makes them visible to the (auto-reloading)
//! reader.
//!
//! ## Scoping
//!
//! Every query is `AND`-combined with a required visibility/permission filter
//! (see [`SearchScope`]) so a caller can never receive a hit for a private
//! repository it may not read.

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, Value, INDEXED, STORED, STRING, TEXT,
};
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};

use crate::document::{DocKind, IndexDocument};

/// Errors surfaced by the search index. Storage/driver detail is wrapped, never
/// leaked verbatim to API callers.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("search index open error: {0}")]
    Open(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("search query error: {0}")]
    Query(String),
}

/// Result type for index operations.
pub type Result<T> = std::result::Result<T, SearchError>;

/// The set of fields, resolved once at construction.
#[derive(Clone, Copy)]
struct Fields {
    doc_id: Field,
    kind: Field,
    repo_id: Field,
    org: Field,
    repo: Field,
    visibility: Field,
    number: Field,
    oid: Field,
    path: Field,
    title: Field,
    body: Field,
}

/// A single ranked search hit, ready to serialize into an API response.
#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    /// `"repo"`, `"issue"`, `"pull_request"`, `"commit"`, or `"blob"`.
    pub kind: String,
    pub org: String,
    pub repo: String,
    /// Issue/PR number (0 when not applicable).
    pub number: i64,
    /// Commit/blob object id (empty when not applicable).
    pub oid: String,
    /// Blob path (empty when not applicable).
    pub path: String,
    /// Title / summary / repo name.
    pub title: String,
    /// A short plain-text excerpt of the matching body.
    pub snippet: String,
    /// BM25 relevance score (higher is better).
    pub score: f32,
}

/// Who is asking, expressed as a visibility/permission filter. Public documents
/// are always visible; private documents are visible only when their `repo_id`
/// is in `visible_private_repo_ids`.
#[derive(Clone, Debug, Default)]
pub struct SearchScope {
    /// Repository ids (as strings) whose **private** content the caller may read.
    pub visible_private_repo_ids: Vec<String>,
    /// Optional kind filter (only return hits of this kind).
    pub kind: Option<DocKind>,
    /// Optional single-repo filter (only return hits from this repo id).
    pub repo_id: Option<String>,
}

impl SearchScope {
    /// An anonymous caller: public repositories only.
    pub fn anonymous() -> Self {
        Self::default()
    }

    /// A caller who may additionally read these private repositories.
    pub fn with_private_repos(ids: Vec<String>) -> Self {
        Self {
            visible_private_repo_ids: ids,
            ..Self::default()
        }
    }
}

/// A Tantivy full-text index over forge content.
pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter<TantivyDocument>>,
    fields: Fields,
}

/// Writer heap budget. 50 MB is comfortable for a single-node forge and keeps
/// commits fast.
const WRITER_HEAP_BYTES: usize = 50 * 1024 * 1024;

/// Longest body excerpt returned as a snippet.
const SNIPPET_BYTES: usize = 240;

impl SearchIndex {
    fn build_schema() -> (Schema, Fields) {
        let mut sb = Schema::builder();
        // `STRING` = not tokenized (exact term); `TEXT` = tokenized full text.
        let doc_id = sb.add_text_field("doc_id", STRING | STORED);
        let kind = sb.add_text_field("kind", STRING | STORED);
        let repo_id = sb.add_text_field("repo_id", STRING | STORED);
        let org = sb.add_text_field("org", STRING | STORED);
        let repo = sb.add_text_field("repo", STRING | STORED);
        let visibility = sb.add_text_field("visibility", STRING | STORED);
        let number = sb.add_i64_field("number", STORED | INDEXED);
        let oid = sb.add_text_field("oid", STRING | STORED);
        let path = sb.add_text_field("path", STRING | STORED);
        let title = sb.add_text_field("title", TEXT | STORED);
        let body = sb.add_text_field("body", TEXT | STORED);
        let schema = sb.build();
        let fields = Fields {
            doc_id,
            kind,
            repo_id,
            org,
            repo,
            visibility,
            number,
            oid,
            path,
            title,
            body,
        };
        (schema, fields)
    }

    /// Open the index at `dir`, creating it (and the directory) if absent.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| {
            SearchError::Query(format!("creating search index dir {}: {e}", dir.display()))
        })?;
        let (schema, fields) = Self::build_schema();
        let mmap = tantivy::directory::MmapDirectory::open(dir)?;
        let index = Index::open_or_create(mmap, schema)?;
        Self::from_index(index, fields)
    }

    /// An ephemeral in-RAM index (used by tests).
    pub fn in_memory() -> Result<Self> {
        let (schema, fields) = Self::build_schema();
        let index = Index::create_in_ram(schema);
        Self::from_index(index, fields)
    }

    fn from_index(index: Index, fields: Fields) -> Result<Self> {
        let writer = index.writer::<TantivyDocument>(WRITER_HEAP_BYTES)?;
        let reader = index.reader()?;
        Ok(Self {
            index,
            reader,
            writer: Mutex::new(writer),
            fields,
        })
    }

    fn to_tantivy(&self, d: &IndexDocument) -> TantivyDocument {
        let f = &self.fields;
        let mut doc = TantivyDocument::new();
        doc.add_text(f.doc_id, &d.doc_id);
        doc.add_text(f.kind, d.kind.as_str());
        doc.add_text(f.repo_id, &d.repo_id);
        doc.add_text(f.org, &d.org);
        doc.add_text(f.repo, &d.repo);
        doc.add_text(f.visibility, &d.visibility);
        doc.add_i64(f.number, d.number);
        doc.add_text(f.oid, &d.oid);
        doc.add_text(f.path, &d.path);
        doc.add_text(f.title, &d.title);
        doc.add_text(f.body, &d.body);
        doc
    }

    /// Upsert one document and commit so it is immediately searchable. For bulk
    /// loads prefer [`SearchIndex::reindex`], which commits once.
    pub fn upsert(&self, d: &IndexDocument) -> Result<()> {
        {
            let mut writer = self.writer.lock().expect("search writer poisoned");
            writer.delete_term(Term::from_field_text(self.fields.doc_id, &d.doc_id));
            writer.add_document(self.to_tantivy(d))?;
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    /// Upsert many documents in a single committed transaction. Far cheaper than
    /// calling [`SearchIndex::upsert`] per document (one commit, one reload) —
    /// used when indexing a repository's commits and blobs together.
    pub fn upsert_many(&self, docs: &[IndexDocument]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        {
            let mut writer = self.writer.lock().expect("search writer poisoned");
            for d in docs {
                writer.delete_term(Term::from_field_text(self.fields.doc_id, &d.doc_id));
                writer.add_document(self.to_tantivy(d))?;
            }
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    /// Remove one document by its `doc_id` and commit.
    pub fn remove(&self, doc_id: &str) -> Result<()> {
        {
            let mut writer = self.writer.lock().expect("search writer poisoned");
            writer.delete_term(Term::from_field_text(self.fields.doc_id, doc_id));
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    /// Remove every document belonging to a repository (e.g. on repo deletion).
    pub fn remove_repo(&self, repo_id: &str) -> Result<()> {
        {
            let mut writer = self.writer.lock().expect("search writer poisoned");
            writer.delete_term(Term::from_field_text(self.fields.repo_id, repo_id));
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    /// Rebuild the entire index from `docs`, replacing all prior content in a
    /// single committed transaction. Used by the `reindex` command.
    pub fn reindex(&self, docs: impl IntoIterator<Item = IndexDocument>) -> Result<usize> {
        let mut count = 0usize;
        {
            let mut writer = self.writer.lock().expect("search writer poisoned");
            writer.delete_all_documents()?;
            for d in docs {
                writer.add_document(self.to_tantivy(&d))?;
                count += 1;
            }
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(count)
    }

    /// Search `query_text` within `scope`, returning up to `limit` ranked hits.
    ///
    /// The user query is parsed leniently over `title` (boosted) and `body`, then
    /// `AND`-combined with the required scope filter. An empty query returns no
    /// hits (callers should short-circuit before calling).
    pub fn search(
        &self,
        query_text: &str,
        scope: &SearchScope,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let trimmed = query_text.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let f = &self.fields;
        let mut parser = QueryParser::for_index(&self.index, vec![f.title, f.body]);
        parser.set_field_boost(f.title, 3.0);
        // Lenient parsing: malformed operators degrade to a term search instead
        // of erroring, so hostile/odd input never 500s the endpoint.
        let (user_query, _errors) = parser.parse_query_lenient(trimmed);

        let scope_query = self.scope_query(scope);

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (Occur::Must, user_query),
            (Occur::Must, Box::new(scope_query)),
        ];
        if let Some(kind) = scope.kind {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.kind, kind.as_str()),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(repo_id) = &scope.repo_id {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.repo_id, repo_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let query = BooleanQuery::new(clauses);

        let searcher = self.reader.searcher();
        // `order_by_score()` yields a collector whose fruit is `(Score, DocAddress)`.
        let top = searcher.search(&query, &TopDocs::with_limit(limit.max(1)).order_by_score())?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            hits.push(self.hit_from_doc(&doc, score));
        }
        Ok(hits)
    }

    /// Build the required visibility filter: `visibility = public` OR
    /// `repo_id ∈ visible_private_repo_ids`. At least one Should must match.
    fn scope_query(&self, scope: &SearchScope) -> BooleanQuery {
        let f = &self.fields;
        let mut shoulds: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Should,
            Box::new(TermQuery::new(
                Term::from_field_text(f.visibility, "public"),
                IndexRecordOption::Basic,
            )),
        )];
        for id in &scope.visible_private_repo_ids {
            shoulds.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.repo_id, id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        BooleanQuery::new(shoulds)
    }

    fn hit_from_doc(&self, doc: &TantivyDocument, score: f32) -> SearchHit {
        let f = &self.fields;
        let text = |field: Field| -> String {
            doc.get_first(field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let number = doc
            .get_first(f.number)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let body = text(f.body);
        SearchHit {
            kind: text(f.kind),
            org: text(f.org),
            repo: text(f.repo),
            number,
            oid: text(f.oid),
            path: text(f.path),
            title: text(f.title),
            snippet: make_snippet(&body),
            score,
        }
    }
}

/// A cheap, dependency-free excerpt: the first `SNIPPET_BYTES` bytes of the body
/// with whitespace collapsed. (Tantivy's `SnippetGenerator` could highlight the
/// matched terms, but a plain excerpt keeps the API output stable and safe to
/// HTML-escape at the edge.)
fn make_snippet(body: &str) -> String {
    let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= SNIPPET_BYTES {
        return collapsed;
    }
    let mut end = SNIPPET_BYTES;
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &collapsed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_issue(repo_id: &str, n: i64, title: &str, body: &str) -> IndexDocument {
        IndexDocument::issue(repo_id, "acme", "widgets", "public", n, title, body)
    }

    #[test]
    fn indexes_and_finds_a_public_issue() {
        let idx = SearchIndex::in_memory().unwrap();
        idx.upsert(&public_issue("r1", 1, "Login button broken", "clicking does nothing"))
            .unwrap();

        let hits = idx
            .search("login", &SearchScope::anonymous(), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "issue");
        assert_eq!(hits[0].number, 1);
        assert_eq!(hits[0].title, "Login button broken");
    }

    #[test]
    fn upsert_replaces_prior_document() {
        let idx = SearchIndex::in_memory().unwrap();
        idx.upsert(&public_issue("r1", 1, "first title", "alpha"))
            .unwrap();
        idx.upsert(&public_issue("r1", 1, "second title", "beta unique"))
            .unwrap();

        // Old body term no longer matches; there is exactly one doc for #1.
        assert!(idx.search("alpha", &SearchScope::anonymous(), 10).unwrap().is_empty());
        let hits = idx.search("beta", &SearchScope::anonymous(), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "second title");
    }

    #[test]
    fn private_repos_are_hidden_unless_scoped() {
        let idx = SearchIndex::in_memory().unwrap();
        idx.upsert(&IndexDocument::issue(
            "secret", "acme", "vault", "private", 7, "hidden secret", "classified",
        ))
        .unwrap();

        // Anonymous: no access to the private repo.
        assert!(idx
            .search("secret", &SearchScope::anonymous(), 10)
            .unwrap()
            .is_empty());

        // Scoped to that private repo id: visible.
        let scope = SearchScope::with_private_repos(vec!["secret".to_string()]);
        let hits = idx.search("secret", &scope, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].repo, "vault");
    }

    #[test]
    fn title_outranks_body() {
        let idx = SearchIndex::in_memory().unwrap();
        idx.upsert(&public_issue("r1", 1, "unrelated", "mentions rustlang once"))
            .unwrap();
        idx.upsert(&public_issue("r2", 2, "rustlang deep dive", "content"))
            .unwrap();
        let hits = idx.search("rustlang", &SearchScope::anonymous(), 10).unwrap();
        assert_eq!(hits.len(), 2);
        // The title match should rank first thanks to the field boost.
        assert_eq!(hits[0].number, 2);
    }

    #[test]
    fn kind_filter_restricts_results() {
        let idx = SearchIndex::in_memory().unwrap();
        idx.upsert(&public_issue("r1", 1, "shared keyword", "x"))
            .unwrap();
        idx.upsert(&IndexDocument::pull_request(
            "r1", "acme", "widgets", "public", 2, "shared keyword", "y",
        ))
        .unwrap();

        let scope = SearchScope {
            kind: Some(DocKind::PullRequest),
            ..SearchScope::anonymous()
        };
        let hits = idx.search("shared", &scope, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, "pull_request");
    }

    #[test]
    fn reindex_replaces_everything() {
        let idx = SearchIndex::in_memory().unwrap();
        idx.upsert(&public_issue("r1", 1, "stale", "old data"))
            .unwrap();
        let n = idx
            .reindex(vec![public_issue("r1", 2, "fresh", "new data")])
            .unwrap();
        assert_eq!(n, 1);
        assert!(idx.search("stale", &SearchScope::anonymous(), 10).unwrap().is_empty());
        assert_eq!(idx.search("fresh", &SearchScope::anonymous(), 10).unwrap().len(), 1);
    }
}
