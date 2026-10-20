//! Experimental current-entity format. No legacy head, sequence or mutation gate.
//!
//! Stage 2 exposes a reference namespace and Redis transaction substrate. Production
//! mounts remain closed until packed ownership and VFS durability are qualified.

pub mod backend;
pub mod codec;
pub mod data;
pub mod keys;
pub mod lifecycle;
pub mod memory;
pub mod model;
pub mod namespace;
pub mod ownership;
pub mod recovery;
pub mod redis;
pub mod resolver;

#[cfg(test)]
mod namespace_tests;
#[cfg(test)]
pub(crate) mod test_objects;

pub type Result<T> = std::result::Result<T, EntityError>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EntityError {
    #[error("invalid entity operation: {0}")]
    Invalid(String),
    #[error("corrupt entity metadata: {0}")]
    Corrupt(String),
    #[error("entity backend failure (outcome may be unknown): {0}")]
    Backend(String),
    #[error("writer or lifecycle authority fenced")]
    Fenced,
    #[error("affected entity changed; prepare the entire operation again")]
    Conflict,
    #[error("entity not found")]
    NotFound,
    #[error("entity already exists")]
    Exists,
    #[error("permission denied")]
    Permission,
    #[error("not a directory")]
    NotDirectory,
    #[error("is a directory")]
    IsDirectory,
    #[error("directory not empty")]
    NotEmpty,
    #[error("entity busy")]
    Busy,
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("integer overflow")]
    Overflow,
    #[error("allocator exhausted")]
    Exhausted,
}
