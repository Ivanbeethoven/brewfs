//! Complete reverse paths from exact per-inode native and packed indexes.
//! No lower namespace walk and no partial result on quota, cycle or view drift.

use super::*;
use crate::meta::layer::{MetadataMemoryGuard, OwnedPaths};
use crate::workspace_overlay::packed_v3::wire005::{V3BudgetPool, V3OwnedPermit};

const MAX_PAGES: usize = 256;
const MAX_WORK: usize = 4096;
const MAX_DEPTH: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 256 << 10;
const MAX_PENDING_BYTES: usize = 2 << 20;
// Covers the 256 KiB bytes, 4096 Vec headers and the final consumer's bounded
// ancestor component vector. Admission precedes construction and follows the
// returned paths through every awaited permission lookup.
const PATHS_OUTPUT_BYTES: u64 = 1 << 20;

struct PathsOutputOwner {
    _permit: V3OwnedPermit,
    _reader: Option<crate::workspace_overlay::packed_reader_lifecycle::PackedReaderRequestOwner>,
}

impl std::fmt::Debug for PathsOutputOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathsOutputOwner").finish_non_exhaustive()
    }
}

type Name = (i64, Vec<u8>);

struct Limits {
    pages: usize,
    work: usize,
}
impl Limits {
    fn page(&mut self) -> Result<(), MetaError> {
        self.pages += 1;
        if self.pages > MAX_PAGES {
            return Err(exhausted());
        }
        Ok(())
    }
    fn work(&mut self) -> Result<(), MetaError> {
        self.work += 1;
        if self.work > MAX_WORK {
            return Err(exhausted());
        }
        Ok(())
    }
}

struct PathWork {
    inode: i64,
    components: Vec<Vec<u8>>,
    seen: Vec<i64>,
    bytes: usize,
}

fn exhausted() -> MetaError {
    MetaError::Io(std::io::Error::from_raw_os_error(libc::E2BIG))
}

impl<W: WorkspaceStore + 'static> WorkspaceMetaLayer<W> {
    async fn packed_reverse_aliases(
        &self,
        lower: &WorkspacePackedLower,
        authority: &crate::workspace_overlay::catalog::WorkspaceNativeReverseAuthority,
        inode: i64,
        limits: &mut Limits,
    ) -> Result<Vec<Name>, MetaError> {
        let mut candidates = BTreeSet::new();
        for layer in self.chain().await? {
            let mut cursor: Option<Name> = None;
            loop {
                limits.page()?;
                let page = self
                    .store
                    .get_native_reverse_dentry_page(
                        authority,
                        layer.layer_id,
                        inode,
                        cursor
                            .as_ref()
                            .map(|(parent, name)| (*parent, name.as_slice())),
                        lower.budget.clone(),
                    )
                    .await
                    .map_err(workspace_to_meta)?;
                if page.rows.is_empty() {
                    break;
                }
                for row in &page.rows {
                    limits.work()?;
                    let next = (row.parent_ino, row.name.clone());
                    if cursor.as_ref().is_some_and(|old| &next <= old) {
                        return Err(MetaError::Internal(
                            "native reverse cursor did not advance".into(),
                        ));
                    }
                    candidates.insert(next.clone());
                    cursor = Some(next);
                }
            }
        }
        let mut after = None;
        loop {
            limits.page()?;
            self.validate_packed_metadata(lower).await?;
            let page = lower
                .metadata
                .frozen_reverse_names_page_owned(inode, after.as_deref())
                .await?;
            if page.rows.is_empty() {
                break;
            }
            if page
                .after
                .as_ref()
                .is_none_or(|next| after.as_ref().is_some_and(|old| next <= old))
            {
                return Err(MetaError::Internal(
                    "packed reverse cursor did not advance".into(),
                ));
            }
            for (parent, name) in &page.rows {
                limits.work()?;
                candidates.insert((*parent, name.clone()));
            }
            after = page.after.clone();
        }
        let mut effective = Vec::new();
        for (parent, name) in candidates {
            limits.work()?;
            if self
                .resolve_dentry_entry(parent, &name)
                .await?
                .is_some_and(|entry| entry.ino == inode)
            {
                let parent_attr = self
                    .stat(parent)
                    .await?
                    .ok_or(MetaError::NotFound(parent))?;
                if parent_attr.kind != FileType::Dir {
                    return Err(MetaError::NotDirectory(parent));
                }
                effective.push((parent, name));
            }
        }
        Ok(effective)
    }

    pub(super) async fn packed_paths_bytes(&self, target: i64) -> Result<Vec<Vec<u8>>, MetaError> {
        // Compatibility callers take the bytes only. Consumers that await more
        // metadata work must retain the owned form instead.
        Ok(self.packed_paths_bytes_owned(target).await?.paths)
    }

    pub(super) async fn packed_paths_bytes_owned(
        &self,
        target: i64,
    ) -> Result<OwnedPaths, MetaError> {
        let lower = self
            .packed_lower()
            .ok_or_else(|| MetaError::Internal("missing packed path lower".into()))?;
        let _permit: V3OwnedPermit = lower
            .budget
            .admit(&[(V3BudgetPool::Metadata, 16 << 20)])
            .map_err(packed_lower::budget_to_meta)?;
        let output_permit = lower
            .budget
            .admit(&[(V3BudgetPool::Output, PATHS_OUTPUT_BYTES)])
            .map_err(packed_lower::budget_to_meta)?;
        let output_reader = lower
            .authority
            .retain_reader_request()
            .map_err(workspace_to_meta)?;
        let guard: MetadataMemoryGuard = Arc::new(PathsOutputOwner {
            _permit: output_permit,
            _reader: output_reader,
        });
        let fence = self.packed_metadata_fence().await?;
        let mut limits = Limits { pages: 0, work: 0 };
        if self.stat(target).await?.is_none() {
            lower
                .budget
                .admit(&[])
                .map_err(packed_lower::budget_to_meta)?;
            self.validate_packed_metadata_fence(fence).await?;
            lower
                .budget
                .admit(&[])
                .map_err(packed_lower::budget_to_meta)?;
            return Ok(OwnedPaths {
                paths: Vec::new(),
                guard: Some(guard),
            });
        }
        if target == self.root_ino() {
            lower
                .budget
                .admit(&[])
                .map_err(packed_lower::budget_to_meta)?;
            self.validate_packed_metadata_fence(fence).await?;
            lower
                .budget
                .admit(&[])
                .map_err(packed_lower::budget_to_meta)?;
            return Ok(OwnedPaths {
                paths: vec![b"/".to_vec()],
                guard: Some(guard),
            });
        }
        let authority = self
            .store
            .get_native_reverse_authority(
                fence
                    .as_ref()
                    .ok_or_else(|| MetaError::Internal("missing packed path fence".into()))?
                    .1
                    .as_slice(),
                lower.budget.clone(),
            )
            .await
            .map_err(workspace_to_meta)?;
        let mut cached = BTreeMap::<i64, Vec<Name>>::new();
        let mut pending = vec![PathWork {
            inode: target,
            components: Vec::new(),
            seen: Vec::new(),
            bytes: 128,
        }];
        let mut pending_bytes = 128usize;
        let mut output = BTreeSet::new();
        let mut output_bytes = 0usize;
        while let Some(work) = pending.pop() {
            limits.work()?;
            pending_bytes -= work.bytes;
            if work.seen.contains(&work.inode) || work.seen.len() >= MAX_DEPTH {
                return Err(MetaError::Internal(
                    "packed effective ancestor chain is cyclic/overlong".into(),
                ));
            }
            if let std::collections::btree_map::Entry::Vacant(entry) = cached.entry(work.inode) {
                let aliases = self
                    .packed_reverse_aliases(lower, &authority, work.inode, &mut limits)
                    .await?;
                entry.insert(aliases);
            }
            for (parent, name) in &cached[&work.inode] {
                limits.work()?;
                let path_bytes = name.len()
                    + 1
                    + work
                        .components
                        .iter()
                        .map(|part| part.len() + 1)
                        .sum::<usize>();
                if path_bytes > MAX_OUTPUT_BYTES {
                    return Err(exhausted());
                }
                if *parent == self.root_ino() {
                    if output.len() >= MAX_WORK || output_bytes + path_bytes > MAX_OUTPUT_BYTES {
                        return Err(exhausted());
                    }
                    let mut path = Vec::with_capacity(path_bytes);
                    path.push(b'/');
                    path.extend_from_slice(name);
                    for part in work.components.iter().rev() {
                        path.push(b'/');
                        path.extend_from_slice(part);
                    }
                    if output.insert(path) {
                        output_bytes += path_bytes;
                    }
                } else {
                    let bytes = work.bytes
                        + name.len()
                        + 1
                        + std::mem::size_of::<Vec<u8>>()
                        + std::mem::size_of::<i64>();
                    if pending_bytes + bytes > MAX_PENDING_BYTES {
                        return Err(exhausted());
                    }
                    let mut components = work.components.clone();
                    components.push(name.clone());
                    let mut seen = work.seen.clone();
                    seen.push(work.inode);
                    pending.push(PathWork {
                        inode: *parent,
                        components,
                        seen,
                        bytes,
                    });
                    pending_bytes += bytes;
                }
            }
        }
        lower
            .budget
            .admit(&[])
            .map_err(packed_lower::budget_to_meta)?;
        self.validate_packed_metadata_fence(fence).await?;
        self.store
            .confirm_native_reverse_authority(&authority, lower.budget.clone())
            .await
            .map_err(workspace_to_meta)?;
        lower
            .budget
            .admit(&[])
            .map_err(packed_lower::budget_to_meta)?;
        Ok(OwnedPaths {
            paths: output.into_iter().collect(),
            guard: Some(guard),
        })
    }
}
