//! Bucket/key to filesystem path mapping.
//!
//! See `doc/protocols/s3-gateway.md` §3 (bucket models) and §5 (directory
//! semantics).

use std::fmt;

use crate::posix::NAME_MAX;

const SYS_DIR_NAME: &str = ".brewfs.sys";

/// How buckets map onto the volume namespace.
#[derive(Debug, Clone)]
pub enum BucketMode {
    /// The whole volume is exposed as a single bucket.
    Single { bucket: String },
    /// Top-level directories of the volume are exposed as buckets.
    Multi,
}

impl fmt::Display for BucketMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BucketMode::Single { bucket } => write!(f, "single:{bucket}"),
            BucketMode::Multi => write!(f, "multi"),
        }
    }
}

/// Errors produced while mapping an S3 bucket/key pair onto a path.
#[derive(Debug)]
pub enum PathError {
    /// Bucket name is not a valid S3 bucket name, or is not served by this gateway.
    InvalidBucket(String),
    /// Object key cannot be represented as a path (empty, escapes the bucket root, ...).
    InvalidKey(String),
    /// Object key overlaps the gateway's internal namespace.
    ReservedKey(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::InvalidBucket(b) => write!(f, "invalid bucket name: {b}"),
            PathError::InvalidKey(k) => write!(f, "invalid object key: {k}"),
            PathError::ReservedKey(k) => write!(f, "reserved object key: {k}"),
        }
    }
}

impl std::error::Error for PathError {}

/// Validates a name against the S3 bucket naming rules.
pub fn is_valid_bucket_name(name: &str) -> bool {
    s3s::path::check_bucket_name(name)
}

/// Validates one key component (directory or file name).
fn valid_component(comp: &str) -> bool {
    if comp.is_empty() || comp == "." || comp == ".." {
        return false;
    }
    if comp.len() > NAME_MAX {
        return false;
    }
    !comp.contains('\0')
}

/// Maps a bucket name to the volume path of the bucket root.
///
/// In single-bucket mode this is always `/`; the bucket name itself is only
/// validated, not mapped. In multi-bucket mode it is `/<bucket>`.
pub fn bucket_root(mode: &BucketMode, bucket: &str) -> Result<String, PathError> {
    match mode {
        BucketMode::Single { bucket: expected } => {
            if bucket == expected {
                Ok("/".to_string())
            } else {
                Err(PathError::InvalidBucket(bucket.to_string()))
            }
        }
        BucketMode::Multi => {
            if !is_valid_bucket_name(bucket) || bucket == SYS_DIR_NAME {
                return Err(PathError::InvalidBucket(bucket.to_string()));
            }
            Ok(format!("/{bucket}"))
        }
    }
}

/// Validates the gateway namespace boundary shared by object keys and list prefixes.
pub fn validate_key_namespace(mode: &BucketMode, key: &str) -> Result<(), PathError> {
    if matches!(mode, BucketMode::Single { .. })
        && key
            .trim_end_matches('/')
            .split('/')
            .next()
            .is_some_and(|component| component == SYS_DIR_NAME)
    {
        return Err(PathError::ReservedKey(key.to_string()));
    }
    Ok(())
}

/// Maps a bucket + object key onto an absolute volume path.
///
/// `key` may end with `/` to denote an explicit directory object; the returned
/// path then points at the directory itself.
pub fn object_path(mode: &BucketMode, bucket: &str, key: &str) -> Result<String, PathError> {
    let root = bucket_root(mode, bucket)?;
    if !s3s::path::check_key(key) {
        return Err(PathError::InvalidKey(key.to_string()));
    }
    validate_key_namespace(mode, key)?;
    if key.starts_with('/') {
        return Err(PathError::InvalidKey(key.to_string()));
    }
    if key.is_empty() {
        // An empty key addresses the bucket root itself.
        return Ok(root);
    }
    let trimmed = key.strip_suffix('/').unwrap_or(key);
    if trimmed.is_empty() {
        return Err(PathError::InvalidKey(key.to_string()));
    }
    for comp in trimmed.split('/') {
        if !valid_component(comp) {
            return Err(PathError::InvalidKey(key.to_string()));
        }
    }
    if root.ends_with('/') {
        Ok(format!("{root}{trimmed}"))
    } else {
        Ok(format!("{root}/{trimmed}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_name_rules() {
        assert!(is_valid_bucket_name("abc"));
        assert!(is_valid_bucket_name("my-bucket-1"));
        assert!(is_valid_bucket_name("a.b.c"));
        assert!(!is_valid_bucket_name("ab"));
        assert!(!is_valid_bucket_name("UPPER"));
        assert!(!is_valid_bucket_name("a..b"));
        assert!(!is_valid_bucket_name("-abc"));
        assert!(!is_valid_bucket_name("abc-"));
        assert!(!is_valid_bucket_name("192.168.1.1"));
        assert!(!is_valid_bucket_name("xn--bucket"));
    }

    #[test]
    fn internal_system_bucket_is_reserved_in_multi_mode() {
        assert!(matches!(
            bucket_root(&BucketMode::Multi, ".brewfs.sys"),
            Err(PathError::InvalidBucket(_))
        ));
        assert!(object_path(&BucketMode::Multi, ".brewfs.sys", "file").is_err());
        assert!(bucket_root(&BucketMode::Multi, "..").is_err());
    }

    #[test]
    fn single_bucket_mode() {
        let mode = BucketMode::Single {
            bucket: "vol".to_string(),
        };
        assert_eq!(object_path(&mode, "vol", "a/b.txt").unwrap(), "/a/b.txt");
        assert_eq!(object_path(&mode, "vol", "a/").unwrap(), "/a");
        assert_eq!(object_path(&mode, "vol", "a").unwrap(), "/a");
        assert_eq!(object_path(&mode, "vol", "").unwrap(), "/");
        assert!(object_path(&mode, "other", "a").is_err());
    }

    #[test]
    fn multi_bucket_mode() {
        let mode = BucketMode::Multi;
        assert_eq!(
            object_path(&mode, "data", "a/b.txt").unwrap(),
            "/data/a/b.txt"
        );
        assert!(bucket_root(&mode, "UP").is_err());
    }

    #[test]
    fn rejects_leading_slash_and_reserved_system_keys() {
        let single = BucketMode::Single {
            bucket: "vol".to_string(),
        };
        assert!(matches!(
            object_path(&single, "vol", "/a"),
            Err(PathError::InvalidKey(_))
        ));
        assert!(matches!(
            object_path(&single, "vol", ".brewfs.sys/s3/tmp/file"),
            Err(PathError::ReservedKey(_))
        ));
        assert!(object_path(&BucketMode::Multi, "data", ".brewfs.sys/file").is_ok());
    }

    #[test]
    fn listing_prefix_validation_allows_unrepresentable_paths() {
        let mode = BucketMode::Single {
            bucket: "vol".to_string(),
        };
        assert!(validate_key_namespace(&mode, "/a//../b").is_ok());
        assert!(matches!(
            validate_key_namespace(&mode, ".brewfs.sys/"),
            Err(PathError::ReservedKey(_))
        ));
        assert!(validate_key_namespace(&mode, &"x".repeat(1025)).is_ok());
        assert!(object_path(&mode, "vol", &"x".repeat(1025)).is_err());
    }

    #[test]
    fn rejects_escaping_keys() {
        let mode = BucketMode::Single {
            bucket: "vol".to_string(),
        };
        assert!(object_path(&mode, "vol", "../etc").is_err());
        assert!(object_path(&mode, "vol", "a/../b").is_err());
        assert!(object_path(&mode, "vol", "a//b").is_err());
        assert!(object_path(&mode, "vol", "a//").is_err());
    }
}
