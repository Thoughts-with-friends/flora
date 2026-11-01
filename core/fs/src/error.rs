#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{0}: no such file or directory")]
    NotFound(String),
    #[error("{0}: invalid guest path")]
    InvalidPath(String),
    #[error("{0}: no device mounted")]
    NotMounted(String),
    #[error("{path}: {source}")]
    Io { path: String, source: std::io::Error },
}
