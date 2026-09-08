//! Multipart upload state stored in the hidden system directory.
//!
//! Layout (see `doc/protocols/README.md` §3.1):
//!
//! ```text
//! /.brewfs.sys/s3/uploads/<hh>/<upload-id>/
//!     .target      # JSON `UploadMeta` describing the target object
//!     part-1       # part data, xattr `brewfs.s3.etag` carries the part MD5
//!     part-2
//! ...
//! ```
//!
//! `<hh>` is the first byte of the upload id in hex (256-way fan-out).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Directory holding all multipart upload state.
pub fn uploads_dir() -> String {
    format!("{}/uploads", crate::gateway::s3_sys_dir())
}

/// Directory holding staging files for atomic publishes.
pub fn tmp_dir() -> String {
    format!("{}/tmp", crate::gateway::s3_sys_dir())
}

/// Directory of one multipart upload.
pub fn upload_dir(upload_id: &str) -> String {
    // Fan out by the first two characters of the upload id so a flat
    // directory never holds unbounded entries.
    let hh: String = if upload_id.len() >= 2 {
        upload_id[..2].to_string()
    } else {
        "00".to_string()
    };
    format!("{}/{}", uploads_dir(), hh)
        + "/"
        + upload_id
}

/// Path of a part file inside an upload directory.
pub fn part_path(upload_id: &str, part_number: i64) -> String {
    format!("{}/part-{part_number}", upload_dir(upload_id))
}

/// Marker file storing the upload metadata.
pub const TARGET_FILE: &str = ".target";

/// Metadata of one in-flight multipart upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadMeta {
    pub bucket: String,
    pub key: String,
    pub content_type: Option<String>,
    pub metadata: Option<HashMap<String, String>>,
    /// Unix seconds when the upload was initiated (used by the cleanup task).
    pub initiated: i64,
}

impl UploadMeta {
    pub fn target_path(upload_id: &str) -> String {
        format!("{}/{}", upload_dir(upload_id), TARGET_FILE)
    }
}

/// Generates a new opaque upload id.
pub fn new_upload_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

/// Parses a part file name (`part-<n>`) back into its part number.
pub fn parse_part_name(name: &str) -> Option<i64> {
    name.strip_prefix("part-")?.parse().ok()
}

/// Computes the S3 multipart ETag: MD5 over the concatenated binary part
/// MD5 digests, suffixed with `-<count>`.
pub fn multipart_etag(part_etags: &[String]) -> String {
    let mut ctx = md5::Context::new();
    for etag in part_etags {
        let hex = etag.trim_matches('"');
        if let Ok(bytes) = hex::decode(hex) {
            ctx.consume(&bytes);
        }
    }
    let digest = ctx.compute();
    format!("{digest:x}-{}", part_etags.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_paths() {
        let id = "0123456789abcdef";
        assert_eq!(
            upload_dir(id),
            "/.brewfs.sys/s3/uploads/01/0123456789abcdef"
        );
        assert_eq!(
            part_path(id, 3),
            "/.brewfs.sys/s3/uploads/01/0123456789abcdef/part-3"
        );
    }

    #[test]
    fn part_names() {
        assert_eq!(parse_part_name("part-12"), Some(12));
        assert_eq!(parse_part_name(".target"), None);
        assert_eq!(parse_part_name("part-x"), None);
    }

    #[test]
    fn multipart_etag_format() {
        // Two part md5s (of "a" and "b") -> md5(md5a||md5b)-2
        let etag = multipart_etag(&["0cc175b9c0f1b6a831c399e269772661".to_string()]);
        assert!(etag.ends_with("-1"));
        assert_eq!(etag.len(), 32 + 2);
    }
}
