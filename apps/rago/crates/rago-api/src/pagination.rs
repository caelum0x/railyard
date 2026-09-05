//! Pagination — faithful port of the `Pagination` concern's `meta` shape.

use serde::Deserialize;
use serde_json::{json, Value};

/// Default page size (`Pagination::PER_PAGE`).
pub const PER_PAGE: i64 = 100;

/// `?page=&per_page=` query params.
#[derive(Debug, Deserialize)]
pub struct PageParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

impl PageParams {
    pub fn page(&self) -> i64 {
        self.page.filter(|p| *p > 0).unwrap_or(1)
    }
    pub fn per_page(&self) -> i64 {
        self.per_page.filter(|p| *p > 0).unwrap_or(PER_PAGE)
    }
    pub fn offset(&self) -> i64 {
        (self.page() - 1) * self.per_page()
    }
}

/// `pagination_metadata(records)` — the `meta` object for a collection.
pub fn meta(page: i64, per_page: i64, total_count: i64) -> Value {
    let total_pages = if total_count > 0 {
        (total_count as f64 / per_page as f64).ceil() as i64
    } else {
        0
    };
    let next_page = if page < total_pages { Some(page + 1) } else { None };
    let prev_page = if page > 1 { Some(page - 1) } else { None };
    json!({
        "current_page": if total_count > 0 { page } else { 0 },
        "next_page": next_page,
        "prev_page": prev_page,
        "total_pages": total_pages,
        "total_count": total_count,
    })
}
