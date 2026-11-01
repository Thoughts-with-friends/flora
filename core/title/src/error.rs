use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum TitleError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{0}: not a title directory or .rpx file")]
    Unrecognised(PathBuf),
    #[error("{0}: no RPX found in code/")]
    NoRpx(PathBuf),
    #[error("{path}: invalid XML: {message}")]
    Xml { path: PathBuf, message: String },
    #[error("not a WUX image")]
    BadWuxMagic,
    #[error("WUX image is truncated or corrupt")]
    CorruptWux,
}

impl TitleError {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}
