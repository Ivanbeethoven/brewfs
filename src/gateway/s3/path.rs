//! Bucket/key to filesystem path mapping.
//!
//! See `doc/protocols/s3-gateway.md` §3 (bucket models) and §5 (directory
//! semantics).

use std::fmt;

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
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::InvalidBucket(b) => write!(f, "invalid bucket name: {b}"),
            PathError::InvalidKey(k) => write!(f, "invalid object key: {k}"),
        }
    }
}

impl std::error::Error for PathError {}

/// Validates a name against the S3 bucket naming rules.
pub fn is_valid_bucket_name(name: &str) -> bool {
    let len = name.len();
    if !(3..=63).contains(&len) {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[len - 1].is_ascii_lowercase() && !bytes[len - 1].is_ascii_digit() {
        return false;
    }
    let mut prev_dot = false;
    for &b in bytes {
        let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.';
        if !ok {
            return false;
        }
        if b == b'.' {
            if prev_dot {
                return false; // ".." is not allowed
            }
            prev_dot = true;
        } else {
            prev_dot = false;
        }
    }
    // Reject IPv4-address-like names (not allowed for S3 buckets).
    let looks_like_ip = name
        .split('.')
        .all(|part| part.parse::<u16>().is_ok() && !part.is_empty())
        && name.split('.').count() == 4;
    !looks_like_ip
}

/// Validates one key component (directory or file name).
fn valid_component(comp: &str) -> bool {
    if comp.is_empty() || comp == "." || comp == ".." {
        return false;
    }
    if comp.len() > 255 {
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
            if !is_valid_bucket_name(bucket) {
                return Err(PathError::InvalidBucket(bucket.to_string()));
            }
            Ok(format!("/{bucket}"))
        }
    }
}

/// Maps a bucket + object key onto an absolute volume path.
///
/// `key` may end with `/` to denote an explicit directory object; the returned
/// path then points at the directory itself.
pub fn object_path(mode: &BucketMode, bucket: &str, key: &str) -> Result<String, PathError> {
    let root = bucket_root(mode, bucket)?;
    let key = key.strip_prefix('/').unwrap_or(key);
    if key.is_empty() {
        // An empty key addresses the bucket root itself.
        return Ok(root);
    }
    let _is_dir_object = key.ends_with('/');
    let trimmed = key.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(PathError::InvalidKey(key.to_string()));
    }
    for comp in trimmed.split('/') {
        if !valid_component(comp) {
            return Err(PathError::InvalidKey(key.to_string()));
        }
    }
    if trimmed.is_empty() {
        return Ok(root);
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
    fn rejects_escaping_keys() {
        let mode = BucketMode::Single {
            bucket: "vol".to_string(),
        };
        assert!(object_path(&mode, "vol", "../etc").is_err());
        assert!(object_path(&mode, "vol", "a/../b").is_err());
        assert!(object_path(&mode, "vol", "a//b").is_err());
    }
}
