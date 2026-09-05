use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error("enum decode error: {0}")]
    Enum(String),
}

impl DbError {
    pub fn enum_err(e: rago_core::enums::EnumError) -> Self {
        DbError::Enum(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, DbError>;
