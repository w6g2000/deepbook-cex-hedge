use thiserror::Error;

#[derive(Debug, Error)]
pub enum LendingError {
    #[error("invalid amount: {0}")]
    InvalidAmount(f64),
}
