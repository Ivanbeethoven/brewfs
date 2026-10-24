//! Flat-key listing over the hierarchical namespace.
//!
//! Emulates the S3 list semantics (prefix / delimiter / max-keys / marker)
//! on top of a directory walk. See `doc/protocols/s3-gateway.md` §4/§5.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use s3s::S3Result;

use crate::meta::store::{DirEntry, FileType};

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
    /// Hide explicit directory objects from the object stream.
    pub hide_dir_objects: bool,
}

impl ListQuery {
    fn common_prefix(&self, key: &str) -> Option<String> {
        let delimiter = self.delimiter.as_deref().filter(|d| !d.is_empty())?;
        let relative = key.strip_prefix(&self.prefix)?;
        let end = self.prefix.len() + relative.find(delimiter)? + delimiter.len();
        Some(key[..end].to_string())
    }
}

/// Only the namespace cursor and directory visibility are needed during the
/// walk. Attributes and ETags are loaded for the returned objects afterwards.
#[async_trait]
pub(crate) trait ListingSource: Sync {
    async fn read_page(&self, ino: i64, start: &str, limit: usize) -> S3Result<Vec<DirEntry>>;
    async fn lookup_directory(&self, ino: i64, name: &str) -> S3Result<Option<DirEntry>>;
    async fn directory_object(&self, ino: i64) -> bool;
    async fn load_object(&self, ino: i64, object: &mut ListedObject);
}

enum WalkItem {
    Directory {
        ino: i64,
        parent: String,
        start: String,
    },
    Entry {
        entry: DirEntry,
        parent: String,
    },
}

/// First string after all strings beginning with `prefix`, in UTF-8 order.
fn prefix_end(prefix: &str) -> Option<String> {
    let mut end = prefix.to_string();
    while let Some(last) = end.pop() {
        let mut next = last as u32 + 1;
        if next == 0xd800 {
            next = 0xe000;
        }
        if let Some(next) = char::from_u32(next) {
            end.push(next);
            return Some(end);
        }
    }
    None
}

pub(crate) async fn collect_page(
    source: &impl ListingSource,
    root: i64,
    hide_system_dir: bool,
    query: ListQuery,
) -> S3Result<ListResult> {
    if query.max_keys == 0 {
        return Ok(ListResult::default());
    }
    let ceiling = prefix_end(&query.prefix);
    if ceiling
        .as_ref()
        .is_some_and(|end| query.start_after >= *end)
    {
        return Ok(ListResult::default());
    }
    let batch_size = query.max_keys.saturating_add(1).min(64);
    let mut floor = query.prefix.clone().max(query.start_after.clone());
    let mut pending = BTreeMap::from([(
        String::new(),
        WalkItem::Directory {
            ino: root,
            parent: String::new(),
            start: String::new(),
        },
    )]);
    let mut collector = ListCollector::new(query.clone());
    let mut inodes = BTreeMap::new();

    while let Some((key, item)) = pending.pop_first() {
        if ceiling.as_ref().is_some_and(|end| key >= *end) {
            break;
        }
        match item {
            WalkItem::Directory {
                ino,
                parent,
                mut start,
            } => {
                if (parent < floor && !floor.starts_with(&parent))
                    || !(parent.starts_with(&query.prefix) || query.prefix.starts_with(&parent))
                {
                    continue;
                }
                if let Some(relative) = floor.strip_prefix(&parent) {
                    let component = relative.split('/').next().unwrap_or_default();
                    // Seeking raw names past "a.txt" must not lose directory
                    // "a", whose object key "a/" sorts AFTER "a.txt". Only
                    // proper prefixes followed by a byte below '/' can do this.
                    for (index, ch) in component.char_indices() {
                        let name = &component[..index];
                        if index > 0
                            && ch < '/'
                            && name >= start.as_str()
                            && !(hide_system_dir && parent.is_empty() && name == ".brewfs.sys")
                            && let Some(entry) = source.lookup_directory(ino, name).await?
                        {
                            pending.insert(
                                format!("{parent}{name}/"),
                                WalkItem::Entry {
                                    entry,
                                    parent: parent.clone(),
                                },
                            );
                        }
                    }
                    if relative.contains('/') {
                        if component >= start.as_str()
                            && !(hide_system_dir && parent.is_empty() && component == ".brewfs.sys")
                            && let Some(entry) = source.lookup_directory(ino, component).await?
                        {
                            pending.insert(
                                format!("{parent}{component}/"),
                                WalkItem::Entry {
                                    entry,
                                    parent: parent.clone(),
                                },
                            );
                        }
                        // Seek beyond siblings such as a!0000 when resuming
                        // inside a/. The ancestor was recovered by lookup.
                        start = start.max(format!("{component}/"));
                    } else {
                        start = start.max(component.to_string());
                    }
                }
                let entries = source.read_page(ino, &start, batch_size).await?;
                if entries.len() == batch_size {
                    let start = format!("{}\u{1}", entries.last().unwrap().name);
                    // A cursor is a lower bound on the next name. Keep it in
                    // the same ordered queue as entries: raw name order differs
                    // from object order (a.txt precedes the directory a/).
                    pending.insert(
                        format!("{parent}{start}"),
                        WalkItem::Directory {
                            ino,
                            parent: parent.clone(),
                            start,
                        },
                    );
                }
                for entry in entries {
                    if hide_system_dir && parent.is_empty() && entry.name == ".brewfs.sys" {
                        continue;
                    }
                    let suffix = if entry.kind == FileType::Dir { "/" } else { "" };
                    pending.insert(
                        format!("{parent}{}{suffix}", entry.name),
                        WalkItem::Entry {
                            entry,
                            parent: parent.clone(),
                        },
                    );
                }
            }
            WalkItem::Entry { entry, parent } => {
                let is_dir = entry.kind == FileType::Dir;
                if (key < floor && !(is_dir && floor.starts_with(&key)))
                    || !(key.starts_with(&query.prefix)
                        || (is_dir && query.prefix.starts_with(&key)))
                {
                    continue;
                }
                if let Some(prefix) = query.common_prefix(&key)
                    && prefix <= query.start_after
                {
                    // A marker within a rolled-up prefix excludes that entire
                    // prefix, including descendants beyond the marker.
                    if let Some(end) = prefix_end(&prefix) {
                        floor = floor.max(end);
                    } else {
                        break;
                    }
                    continue;
                }
                let dirobj = is_dir && source.directory_object(entry.ino).await;
                let previous_prefixes = collector.common_prefixes.len();
                collector.push_entry(&parent, &entry.name, is_dir, 0, 0, dirobj, None);
                if (!is_dir || (dirobj && !query.hide_dir_objects))
                    && collector
                        .objects
                        .last()
                        .is_some_and(|object| object.key == key)
                {
                    // At most page size + lookahead; never retain skipped keys.
                    inodes.insert(key.clone(), entry.ino);
                }
                if collector.len() > query.max_keys {
                    break;
                }
                if collector.common_prefixes.len() > previous_prefixes {
                    let prefix = collector.common_prefixes.last().unwrap();
                    if let Some(end) = prefix_end(prefix) {
                        floor = floor.max(end);
                    } else {
                        break;
                    }
                }
                if is_dir && collector.should_descend(&parent, &entry.name, dirobj) {
                    pending.insert(
                        key.clone(),
                        WalkItem::Directory {
                            ino: entry.ino,
                            parent: key,
                            start: String::new(),
                        },
                    );
                }
            }
        }
    }
    let mut result = collector.finish();
    for object in &mut result.objects {
        source.load_object(inodes[&object.key], object).await;
    }
    Ok(result)
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

    fn len(&self) -> usize {
        self.objects.len() + self.common_prefixes.len()
    }

    fn bound_page(&mut self) {
        let limit = self.query.max_keys.saturating_add(1);
        if self.len() <= limit {
            return;
        }
        self.objects.sort_by(|a, b| a.key.cmp(&b.key));
        if self.objects.last().is_some_and(|object| {
            self.common_prefixes
                .last()
                .is_none_or(|prefix| object.key > *prefix)
        }) {
            self.objects.pop();
        } else {
            self.common_prefixes.pop_last();
        }
    }

    /// Records one child entry found under a directory.
    ///
    /// `parent_key` is the parent path relative to the bucket root with a
    /// trailing `/` (empty for the root). `name` is the entry name.
    #[allow(clippy::too_many_arguments)]
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
        let full = if is_dir_object {
            format!("{parent_key}{name}/")
        } else {
            format!("{parent_key}{name}")
        };
        if !self.query.prefix.is_empty() && !full.starts_with(&self.query.prefix) {
            // Not under the requested prefix. A child directory may still
            // contain matching keys, so the caller decides whether to descend.
            return;
        }

        if is_dir_object && self.query.hide_dir_objects {
            return;
        }

        if let Some(cp) = self.query.common_prefix(&full) {
            if cp > self.query.start_after || self.query.start_after.is_empty() {
                self.common_prefixes.insert(cp);
                self.bound_page();
            }
            return;
        }

        if (is_dir && !is_dir_object) || full <= self.query.start_after {
            return;
        }

        self.objects.push(ListedObject {
            key: full,
            size,
            mtime,
            is_dir_object,
            etag,
        });
        self.bound_page();
    }

    /// Whether the walk should descend into the child directory `name`
    /// (only when the prefix could still match below it).
    pub fn should_descend(&self, parent_key: &str, name: &str, is_dir_object: bool) -> bool {
        let subtree = format!("{parent_key}{name}/");
        if self.query.prefix.starts_with(&subtree) {
            return true;
        }
        let Some(relative) = subtree.strip_prefix(&self.query.prefix) else {
            return false;
        };
        match self.query.delimiter.as_deref() {
            Some(delimiter) if !delimiter.is_empty() && relative.contains(delimiter) => {
                !is_dir_object || self.query.hide_dir_objects
            }
            _ => true,
        }
    }

    /// Finishes the listing: sorts entries, applies start-after and max-keys.
    pub fn finish(mut self) -> ListResult {
        if self.query.max_keys == 0 {
            return ListResult::default();
        }

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
    use crate::meta::MetaStore;
    use crate::meta::config::{Config, DatabaseConfig, DatabaseType};
    use crate::meta::stores::DatabaseMetaStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingSource {
        store: DatabaseMetaStore,
        pages: AtomicUsize,
        entries: AtomicUsize,
        lookups: AtomicUsize,
        markers: AtomicUsize,
        attributes: AtomicUsize,
        etags: AtomicUsize,
    }

    impl CountingSource {
        async fn new() -> Self {
            let store = DatabaseMetaStore::from_config(Config {
                database: DatabaseConfig {
                    db_config: DatabaseType::Sqlite {
                        url: "sqlite:file::memory:".to_string(),
                    },
                },
                cache: Default::default(),
                client: Default::default(),
                compact: Default::default(),
            })
            .await
            .unwrap();
            Self {
                store,
                pages: AtomicUsize::new(0),
                entries: AtomicUsize::new(0),
                lookups: AtomicUsize::new(0),
                markers: AtomicUsize::new(0),
                attributes: AtomicUsize::new(0),
                etags: AtomicUsize::new(0),
            }
        }

        async fn add(&self, key: &str) {
            let mut parent = 1;
            let parts: Vec<_> = key.trim_end_matches('/').split('/').collect();
            for (index, part) in parts.iter().enumerate() {
                let last = index == parts.len() - 1;
                let ino = if let Some(ino) = self.store.lookup(parent, part).await.unwrap() {
                    ino
                } else if !last || key.ends_with('/') {
                    self.store.mkdir(parent, part.to_string()).await.unwrap()
                } else {
                    self.store
                        .create_file(parent, part.to_string())
                        .await
                        .unwrap()
                };
                if last {
                    let (name, value): (&str, &[u8]) = if key.ends_with('/') {
                        ("brewfs.s3.dirobj", b"1")
                    } else {
                        ("brewfs.s3.etag", b"etag")
                    };
                    self.store.set_xattr(ino, name, value, 0).await.unwrap();
                }
                parent = ino;
            }
        }

        fn reset(&self) {
            for counter in [
                &self.pages,
                &self.entries,
                &self.lookups,
                &self.markers,
                &self.attributes,
                &self.etags,
            ] {
                counter.store(0, Ordering::Relaxed);
            }
        }
    }

    #[async_trait]
    impl ListingSource for CountingSource {
        async fn read_page(&self, ino: i64, start: &str, limit: usize) -> S3Result<Vec<DirEntry>> {
            self.pages.fetch_add(1, Ordering::Relaxed);
            let entries = self.store.readdir_page(ino, start, limit).await.unwrap();
            assert!(entries.len() <= limit);
            self.entries.fetch_add(entries.len(), Ordering::Relaxed);
            Ok(entries)
        }

        async fn lookup_directory(&self, ino: i64, name: &str) -> S3Result<Option<DirEntry>> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            Ok(self
                .store
                .lookup_with_attr(ino, name)
                .await
                .unwrap()
                .filter(|(_, attr)| attr.kind == FileType::Dir)
                .map(|(ino, attr)| DirEntry {
                    ino,
                    name: name.to_string(),
                    kind: attr.kind,
                }))
        }

        async fn directory_object(&self, ino: i64) -> bool {
            self.markers.fetch_add(1, Ordering::Relaxed);
            self.store
                .get_xattr(ino, "brewfs.s3.dirobj")
                .await
                .unwrap()
                .is_some()
        }

        async fn load_object(&self, ino: i64, object: &mut ListedObject) {
            self.attributes.fetch_add(1, Ordering::Relaxed);
            let attr = self.store.stat(ino).await.unwrap().unwrap();
            object.size = attr.size;
            object.mtime = attr.mtime;
            if !object.is_dir_object {
                self.etags.fetch_add(1, Ordering::Relaxed);
                object.etag = self
                    .store
                    .get_xattr(ino, "brewfs.s3.etag")
                    .await
                    .unwrap()
                    .map(|value| String::from_utf8(value).unwrap());
            }
        }
    }

    fn page_keys(result: &ListResult) -> Vec<String> {
        let mut keys: Vec<_> = result
            .objects
            .iter()
            .map(|o| o.key.clone())
            .chain(result.common_prefixes.iter().cloned())
            .collect();
        keys.sort();
        keys
    }

    #[tokio::test]
    async fn first_and_later_pages_bound_metadata_work() {
        let source = CountingSource::new().await;
        for n in 0..1024 {
            source.add(&format!("key{n:04}")).await;
        }
        for (marker, expected, truncated) in [
            ("", "key0000", true),
            ("key0512", "key0513", true),
            ("key1022", "key1023", false),
        ] {
            source.reset();
            let page = collect_page(
                &source,
                1,
                true,
                ListQuery {
                    max_keys: 1,
                    start_after: marker.to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(page_keys(&page), [expected]);
            assert_eq!(page.next_marker, expected);
            assert_eq!(page.is_truncated, truncated);
            assert_eq!(page.objects[0].etag.as_deref(), Some("etag"));
            assert!(source.pages.load(Ordering::Relaxed) <= 2);
            assert!(source.entries.load(Ordering::Relaxed) <= 4);
            assert_eq!(source.lookups.load(Ordering::Relaxed), 0);
            assert_eq!(source.markers.load(Ordering::Relaxed), 0);
            assert_eq!(source.attributes.load(Ordering::Relaxed), 1);
            assert_eq!(source.etags.load(Ordering::Relaxed), 1);
        }
        source.reset();
        let page = collect_page(&source, 1, true, ListQuery::default())
            .await
            .unwrap();
        assert!(page_keys(&page).is_empty());
        assert!(!page.is_truncated);
        assert_eq!(source.pages.load(Ordering::Relaxed), 0);
        for (prefix, marker) in [("absent", ""), ("key000", "key0512")] {
            source.reset();
            let page = collect_page(
                &source,
                1,
                true,
                ListQuery {
                    prefix: prefix.to_string(),
                    start_after: marker.to_string(),
                    max_keys: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(page_keys(&page).is_empty());
            assert!(source.pages.load(Ordering::Relaxed) <= 1);
            assert!(source.entries.load(Ordering::Relaxed) <= 2);
            assert_eq!(source.attributes.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test]
    async fn cursor_matches_flat_listing_across_prefixes_delimiters_and_hidden_directories() {
        let source = CountingSource::new().await;
        let keys = [
            "a!/",
            "a!.txt",
            "a!/x",
            "a.txt",
            "a/",
            "a/child",
            "a/implicit/nested",
            "a0",
            "b/",
            "b/child",
            "empty/",
            "é/child",
            "雪",
        ];
        for key in keys {
            source.add(key).await;
        }
        source.add(".brewfs.sys/private").await;
        source
            .store
            .mkdir(1, "implicit-empty".to_string())
            .await
            .unwrap();
        for prefix in ["", "a", "a/", "a/imp", "absent", "é"] {
            for delimiter in [None, Some("/"), Some("."), Some("!/"), Some("")] {
                for hide_dir_objects in [false, true] {
                    let mut expected = BTreeSet::new();
                    let query = ListQuery {
                        prefix: prefix.to_string(),
                        delimiter: delimiter.map(str::to_string),
                        hide_dir_objects,
                        max_keys: 1,
                        ..Default::default()
                    };
                    for key in keys {
                        if key.starts_with(prefix) && !(hide_dir_objects && key.ends_with('/')) {
                            expected.insert(
                                query.common_prefix(key).unwrap_or_else(|| key.to_string()),
                            );
                        }
                    }
                    let expected: Vec<_> = expected.into_iter().collect();
                    let mut found = Vec::new();
                    let mut query = query;
                    loop {
                        let page = collect_page(&source, 1, true, query.clone()).await.unwrap();
                        assert!(page_keys(&page).len() <= 1);
                        found.extend(page_keys(&page));
                        if !page.is_truncated {
                            break;
                        }
                        assert!(page.next_marker > query.start_after);
                        query.start_after = page.next_marker;
                        assert!(found.len() <= keys.len());
                    }
                    assert_eq!(found, expected, "query: {query:?}");
                }
            }
        }
        // Arbitrary markers need not be existing keys or previous page tokens.
        let page = collect_page(
            &source,
            1,
            true,
            ListQuery {
                start_after: "a-absent".to_string(),
                max_keys: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page_keys(&page), ["a.txt"]);
        let page = collect_page(
            &source,
            1,
            true,
            ListQuery {
                start_after: "a.txt".to_string(),
                max_keys: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page_keys(&page), ["a/"]);
        let page = collect_page(
            &source,
            1,
            true,
            ListQuery {
                start_after: ".brewfs.sys!cursor".to_string(),
                max_keys: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page_keys(&page), ["a!.txt"]);
    }

    #[tokio::test]
    async fn delimiter_skips_large_subtrees_and_markers_inside_common_prefixes() {
        let source = CountingSource::new().await;
        for n in 0..256 {
            source.add(&format!("a/key{n:04}")).await;
            source.add(&format!("b/key{n:04}")).await;
        }
        source.add("z").await;
        for (marker, expected) in [("", "a/"), ("a/", "b/"), ("a/key0128", "b/")] {
            source.reset();
            let page = collect_page(
                &source,
                1,
                true,
                ListQuery {
                    start_after: marker.to_string(),
                    delimiter: Some("/".to_string()),
                    max_keys: 1,
                    hide_dir_objects: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(page_keys(&page), [expected]);
            assert_eq!(page.next_marker, expected);
            assert!(page.is_truncated);
            assert!(source.pages.load(Ordering::Relaxed) <= 5);
            assert!(source.entries.load(Ordering::Relaxed) <= 8);
            assert!(source.markers.load(Ordering::Relaxed) <= 2);
            assert_eq!(source.attributes.load(Ordering::Relaxed), 0);
            assert_eq!(source.etags.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test]
    async fn seeking_inside_directory_skips_siblings_before_its_slash() {
        let source = CountingSource::new().await;
        source.add("a/first").await;
        source.add("a/second").await;
        source.add("z").await;
        for index in 0..256 {
            source.add(&format!("a!{index:04}")).await;
        }
        for (prefix, marker, expected) in [("a/", "", "a/first"), ("", "a/first", "a/second")] {
            source.reset();
            let page = collect_page(
                &source,
                1,
                true,
                ListQuery {
                    prefix: prefix.to_string(),
                    start_after: marker.to_string(),
                    max_keys: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(page_keys(&page), [expected]);
            assert!(page.is_truncated);
            assert!(source.entries.load(Ordering::Relaxed) <= 4);
            assert!(source.pages.load(Ordering::Relaxed) <= 3);
            assert!(source.lookups.load(Ordering::Relaxed) <= 1);
        }
    }

    fn obj(key: &str) -> ListedObject {
        ListedObject {
            key: key.to_string(),
            size: 1,
            mtime: 0,
            is_dir_object: false,
            etag: None,
        }
    }

    #[test]
    fn collector_retains_only_one_page_and_lookahead() {
        let mut c = ListCollector::new(ListQuery {
            max_keys: 1,
            ..Default::default()
        });
        for n in (0..4096).rev() {
            c.push_entry("", &format!("key-{n:04}"), false, 1, 0, false, None);
            assert!(c.objects.len() + c.common_prefixes.len() <= 2);
        }
        let page = c.finish();
        assert_eq!(page.objects[0].key, "key-0000");
        assert_eq!(page.next_marker, "key-0000");
        assert!(page.is_truncated);
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
    fn zero_limit_is_not_truncated() {
        let mut c = ListCollector::new(ListQuery {
            max_keys: 0,
            ..Default::default()
        });
        c.objects = vec![obj("a")];

        let r = c.finish();
        assert!(r.objects.is_empty());
        assert!(!r.is_truncated);
        assert!(r.next_marker.is_empty());
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
    fn invisible_directories_do_not_consume_page_slots() {
        let mut c = ListCollector::new(ListQuery {
            max_keys: 2,
            ..Default::default()
        });
        c.push_entry("", "implicit", true, 0, 0, false, None);
        c.push_entry("implicit/", "a.txt", false, 1, 0, false, None);
        c.push_entry("", "z.txt", false, 1, 0, false, None);
        c.push_entry("", "zz.txt", false, 1, 0, false, None);

        let r = c.finish();
        let keys: Vec<_> = r.objects.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["implicit/a.txt", "z.txt"]);
        assert!(r.is_truncated);
        assert_eq!(r.next_marker, "z.txt");
    }

    #[test]
    fn hidden_directory_objects_do_not_consume_page_slots() {
        let mut c = ListCollector::new(ListQuery {
            max_keys: 1,
            hide_dir_objects: true,
            ..Default::default()
        });
        c.push_entry("", "a-dir", true, 0, 0, true, None);
        c.push_entry("", "b.txt", false, 1, 0, false, None);
        c.push_entry("", "c.txt", false, 1, 0, false, None);

        let r = c.finish();
        assert_eq!(r.objects.len(), 1);
        assert_eq!(r.objects[0].key, "b.txt");
        assert!(r.is_truncated);
        assert_eq!(r.next_marker, "b.txt");
    }

    #[test]
    fn invisible_directories_still_produce_common_prefixes() {
        let mut c = ListCollector::new(ListQuery {
            delimiter: Some("/".to_string()),
            max_keys: 10,
            hide_dir_objects: true,
            ..Default::default()
        });
        c.push_entry("", "photos", true, 0, 0, false, None);
        c.push_entry("photos/", "image.jpg", false, 1, 0, false, None);

        let r = c.finish();
        assert!(r.objects.is_empty());
        assert_eq!(
            r.common_prefixes.into_iter().collect::<Vec<_>>(),
            ["photos/"]
        );
    }

    #[test]
    fn directory_object_uses_its_trailing_slash_for_matching() {
        let mut prefixed = ListCollector::new(ListQuery {
            prefix: "empty-dir/".to_string(),
            max_keys: 10,
            ..Default::default()
        });
        prefixed.push_entry("", "empty-dir", true, 0, 0, true, None);
        let prefixed = prefixed.finish();
        assert_eq!(prefixed.objects[0].key, "empty-dir/");

        let mut delimited = ListCollector::new(ListQuery {
            delimiter: Some("/".to_string()),
            max_keys: 10,
            ..Default::default()
        });
        delimited.push_entry("", "empty-dir", true, 0, 0, true, None);
        let delimited = delimited.finish();
        assert!(delimited.objects.is_empty());
        assert_eq!(
            delimited.common_prefixes.into_iter().collect::<Vec<_>>(),
            ["empty-dir/"]
        );

        let mut hidden_delimited = ListCollector::new(ListQuery {
            delimiter: Some("/".to_string()),
            max_keys: 10,
            hide_dir_objects: true,
            ..Default::default()
        });
        hidden_delimited.push_entry("", "empty-dir", true, 0, 0, true, None);
        let hidden_delimited = hidden_delimited.finish();
        assert!(hidden_delimited.objects.is_empty());
        assert!(hidden_delimited.common_prefixes.is_empty());
    }

    #[test]
    fn descend_rules() {
        let c = ListCollector::new(ListQuery {
            prefix: "a/b".to_string(),
            ..Default::default()
        });
        assert!(c.should_descend("", "a", false));
        assert!(!c.should_descend("", "z", false));
        assert!(c.should_descend("a/", "b", false));

        let delimited = ListCollector::new(ListQuery {
            delimiter: Some("/".to_string()),
            ..Default::default()
        });
        assert!(!delimited.should_descend("", "visible", true));
        assert!(delimited.should_descend("", "implicit", false));

        let hidden = ListCollector::new(ListQuery {
            delimiter: Some("/".to_string()),
            hide_dir_objects: true,
            ..Default::default()
        });
        assert!(hidden.should_descend("", "hidden", true));
    }
}
