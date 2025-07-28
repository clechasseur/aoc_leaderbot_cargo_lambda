use std::path::PathBuf;

use aoc_leaderbot_cargo_lambda_metadata::error::MetadataError;
use miette::Diagnostic;
use object::Architecture;
use thiserror::Error;

#[derive(Debug, Diagnostic, Error)]
pub(crate) enum BuildError {
    #[error("binary file for {0} not found, use `cargo lambda {1}` to create it")]
    #[diagnostic()]
    BinaryMissing(String, String),
    #[error("invalid binary architecture: {0:?}")]
    #[diagnostic()]
    InvalidBinaryArchitecture(Architecture),
    #[error("invalid unix file name: {0}")]
    #[diagnostic()]
    InvalidUnixFileName(PathBuf),
    #[error(transparent)]
    #[diagnostic()]
    FailedBuildCommand(#[from] std::io::Error),
    #[error(transparent)]
    #[diagnostic()]
    MetadataError(#[from] MetadataError),
}
