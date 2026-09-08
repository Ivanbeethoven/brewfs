//! Flat-key listing over the hierarchical namespace.
//!
//! Emulates the S3 list semantics (prefix / delimiter / max-keys / marker)
//! on top of a directory walk. See `doc/protocols/s3-gateway.md` §4/§5.

use std::collections::BTreeSet;

/// Parameters of a listing request.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    /// Only keys starting with this prefix are returned.
    pub prefix: String,
    /// Roll up keys sharing a prefix up to (and including) this delimiter
    /// into common prefixes instead of listing them.
    pub delimiter: Option<String>,
    /// Exclusive start point; listing resumes after this key.
    pub start_after: String,
    /// Maximum number of keys + common prefixes returned.
    pub max_keys: usize,
}

/// One listed object entry.
#[derive(Debug, Clone)]
pub struct ListedObject {
    /// Full object key.
    pub key: String,
    /// Object size in bytes (0 for directory objects).
    pub size: u64,
    /// mtime in unix seconds.
    pub mtime: i64,
    /// The entry is an explicit directory object (key ends with `/`).
    pub is_dir_object: bool,
    /// The entry is a directory without the directory-object marker
    /// (implicit directory; normally only used internally).
    pub is_dir: bool,
    /// Stored ETag xattr, if any.
    pub etag: Option<String>,
}

/// Result of a listing.
#[derive(Debug, Clone, Default)]
pub struct ListResult {
    pub objects: Vec<ListedObject>,
    pub common_prefixes: BTreeSet<String>,
    pub is_truncated: bool,
    /// Key of the last returned entry (V1 `NextMarker`; V2 continuation token).
    pub next_marker: String,
}

impl ListResult {
    /// Total number of returned entries (keys + common prefixes).
    pub fn len(&self) -> usize {
        self.objects.len() + self.common_prefixes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Collects entries during a directory walk, applying prefix/delimiter rules.
#[derive(Debug, Default)]
pub struct ListCollector {
    query: ListQuery,
    objects: Vec<ListedObject>,
    common_prefixes: BTreeSet<String>,
}

impl ListCollector {
    pub fn new(query: ListQuery) -> Self {
        Self {
            query,
            objects: Vec::new(),
            common_prefixes: BTreeSet::new(),
        }
    }

    /// Records one child entry found under a directory.
    ///
    /// `parent_key` is the parent path relative to the bucket root with a
    /// trailing `/` (empty for the root). `name` is the entry name.
    pub fn push_entry(
        &mut self,
        parent_key: &str,
        name: &str,
        is_dir: bool,
        size: u64,
        mtime: i64,
        is_dir_object: bool,
        etag: Option<String>,
    ) {
        let full = format!("{parent_key}{name}");
        if !self.query.prefix.is_empty() && !full.starts_with(&self.query.prefix) {
            // Not under the requested prefix. A child directory may still
            // contain matching keys, so the caller decides whether to descend.
            return;
        }

        if let Some(delim) = self.query.delimiter.clone() {
            if let Some(rel) = full.strip_prefix(&self.query.prefix) {
                if let Some(idx) = rel.find(&delim) {
                    let end = self.query.prefix.len() + idx + delim.len();
                    let cp = full[..end].to_string();
                    if cp > self.query.start_after || self.query.start_after.is_empty() {
                        self.common_prefixes.insert(cp);
                    }
                    return;
                }
            }
        }

        let key = if is_dir_object {
            format!("{full}/")
        } else {
            full
        };
        self.objects.push(ListedObject {
            key,
            size,
            mtime,
            is_dir_object,
            is_dir,
            etag,
        });
    }

    /// Whether the walk should descend into the child directory `name`
    /// (only when the prefix could still match below it).
    pub fn should_descend(&self, parent_key: &str, name: &str) -> bool {
        if self.query.delimiter.is_some() {
            // Delimited listings never report keys below a common prefix,
            // but we must still descend far enough to detect the prefix itself
            // (handled via common_prefixes above).
            return true;
        }
        let full = format!("{parent_key}{name}");
        if self.query.prefix.is_empty() {
            return true;
        }
        if full.starts_with(&self.query.prefix) {
            return true;
        }
        // Descend only if this directory can be a proper prefix of the target
        // prefix (e.g. prefix "a/b" and directory "a").
        self.query.prefix.starts_with(&full)
    }

    /// Finishes the listing: sorts entries, applies start-after and max-keys.
    pub fn finish(mut self) -> ListResult {
        // Merge objects and common prefixes into one lexically sorted stream
        // (S3 returns keys and common prefixes interleaved in sorted order).
        self.objects.sort_by(|a, b| a.key.cmp(&b.key));

        let mut merged: Vec<MergedEntry> = Vec::new();
        for cp in &self.common_prefixes {
            merged.push(MergedEntry::Prefix(cp.clone()));
        }
        for o in self.objects.drain(..) {
            merged.push(MergedEntry::Object(o));
        }
        merged.sort_by(|a, b| a.key().cmp(b.key()));

        let mut objects = Vec::new();
        let mut prefixes = BTreeSet::new();
        let mut is_truncated = false;
        let mut next_marker = String::new();

        for entry in merged {
            let key = entry.key().to_string();
            if !self.query.start_after.is_empty() && key <= self.query.start_after {
                continue;
            }
            if objects.len() + prefixes.len() >= self.query.max_keys {
                is_truncated = true;
                break;
            }
            match entry {
                MergedEntry::Object(o) => objects.push(o),
                MergedEntry::Prefix(p) => {
                    prefixes.insert(p);
                }
            }
            next_marker = key;
        }

        ListResult {
            objects,
            common_prefixes: prefixes,
            is_truncated,
            next_marker,
        }
    }
}

enum MergedEntry {
    Object(ListedObject),
    Prefix(String),
}

impl MergedEntry {
    fn key(&self) -> &str {
        match self {
            MergedEntry::Object(o) => &o.key,
            MergedEntry::Prefix(p) => p,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(key: &str) -> ListedObject {
        ListedObject {
            key: key.to_string(),
            size: 1,
            mtime: 0,
            is_dir_object: false,
            is_dir: false,
            etag: None,
        }
    }

    #[test]
    fn finish_sorts_and_limits() {
        let mut c = ListCollector::new(ListQuery {
            max_keys: 2,
            ..Default::default()
        });
        c.objects = vec![obj("b"), obj("a"), obj("c")];
        let r = c.finish();
        assert_eq!(r.objects.len(), 2);
        assert_eq!(r.objects[0].key, "a");
        assert_eq!(r.objects[1].key, "b");
        assert!(r.is_truncated);
        assert_eq!(r.next_marker, "b");
    }

    #[test]
    fn start_after_filters() {
        let mut c = ListCollector::new(ListQuery {
            start_after: "b".to_string(),
            max_keys: 10,
            ..Default::default()
        });
        c.objects = vec![obj("a"), obj("b"), obj("c")];
        let r = c.finish();
        assert_eq!(r.objects.len(), 1);
        assert_eq!(r.objects[0].key, "c");
        assert!(!r.is_truncated);
    }

    #[test]
    fn delimiter_collects_common_prefixes() {
        let mut c = ListCollector::new(ListQuery {
            delimiter: Some("/".to_string()),
            max_keys: 10,
            ..Default::default()
        });
        c.common_prefixes.insert("photos/".to_string());
        c.objects = vec![obj("readme.txt")];
        let r = c.finish();
        assert_eq!(r.common_prefixes.len(), 1);
        assert_eq!(r.objects.len(), 1);
    }

    #[test]
    fn descend_rules() {
        let c = ListCollector::new(ListQuery {
            prefix: "a/b".to_string(),
            ..Default::default()
        });
        assert!(c.should_descend("", "a"));
        assert!(!c.should_descend("", "z"));
        assert!(c.should_descend("a/", "b"));
    }
}
