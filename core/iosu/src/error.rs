#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IosError {
    #[error("{0}: no such IOSU device")]
    NoSuchDevice(String),
    #[error("device handle {0} is not open")]
    BadHandle(u32),
}
