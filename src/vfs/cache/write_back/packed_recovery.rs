//! Bounded, strict recovery inventory used by the packed publication owner.

use super::*;
use tokio::io::AsyncReadExt;

#[cfg(test)]
mod tests;

const MAX_RECOVERY_ROWS: usize = 256;
const MAX_RECOVERY_ENTRIES: usize = 4096;
const MAX_RECOVERY_META_BYTES: usize = 4096;
const MAX_RECOVERY_PATH_BYTES: usize = 4096;
pub(crate) const MAX_PACKED_RECOVERY_SLICE_BYTES: u64 = 64 << 20;

fn account_entry(entries: &mut usize, path: &Path) -> anyhow::Result<()> {
    *entries = entries
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("recovery entry overflow"))?;
    anyhow::ensure!(
        *entries <= MAX_RECOVERY_ENTRIES,
        "packed recovery directory entry cap"
    );
    anyhow::ensure!(
        path.as_os_str().len() <= MAX_RECOVERY_PATH_BYTES,
        "packed recovery path byte cap"
    );
    Ok(())
}

impl FsWriteBackCache {
    pub(crate) fn has_recoverable_records(&self) -> bool {
        !self.recoverable_keys.is_empty()
    }

    async fn push_packed_recovery_record(
        &self,
        path: PathBuf,
        records: &mut Vec<DirtySliceRecord>,
    ) -> anyhow::Result<()> {
        let record = if Self::is_meta_path(&path) {
            let file = fs::File::open(&path).await?;
            let mut bytes = Vec::with_capacity(MAX_RECOVERY_META_BYTES + 1);
            file.take((MAX_RECOVERY_META_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            anyhow::ensure!(
                bytes.len() <= MAX_RECOVERY_META_BYTES,
                "packed recovery metadata byte cap"
            );
            let record: DirtySliceRecord = serde_json::from_slice(&bytes)?;
            if record.volume_scope != self.volume_scope
                || matches!(
                    record.state,
                    DirtySliceState::Committed | DirtySliceState::Obsolete
                )
            {
                return Ok(());
            }
            record
        } else if Self::is_sealed_path(&path) {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow::anyhow!("packed recovery record filename"))?;
            let (_, _, _, scope) = DirtySliceKey::parse_sealed_file_name(name)
                .ok_or_else(|| anyhow::anyhow!("packed recovery sealed filename"))?;
            if scope != self.volume_scope_fingerprint() {
                return Ok(());
            }
            self.sealed_record_from_path(path)
                .ok_or_else(|| anyhow::anyhow!("packed recovery sealed record"))?
        } else {
            return Ok(());
        };
        anyhow::ensure!(records.len() < MAX_RECOVERY_ROWS, "packed recovery row cap");
        anyhow::ensure!(
            !records.iter().any(|known| known.key == record.key),
            "packed recovery duplicate key"
        );
        anyhow::ensure!(
            record.length <= MAX_PACKED_RECOVERY_SLICE_BYTES,
            "packed recovery slice byte cap"
        );
        anyhow::ensure!(
            record.ino == record.key.ino && record.chunk_id == record.key.chunk_id,
            "packed recovery key mismatch"
        );
        anyhow::ensure!(
            record.path.as_os_str().len() <= MAX_RECOVERY_PATH_BYTES,
            "packed recovery data path byte cap"
        );
        let root = fs::canonicalize(&self.root).await?;
        let data_path = fs::canonicalize(&record.path).await?;
        anyhow::ensure!(
            data_path.starts_with(root),
            "packed recovery data path escapes cache"
        );
        let metadata = fs::symlink_metadata(&record.path).await?;
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "packed recovery data is not a regular file"
        );
        anyhow::ensure!(
            metadata.len() == record.length,
            "packed recovery data length mismatch"
        );
        records.push(record);
        Ok(())
    }

    async fn scan_packed_recovery_leaf(
        &self,
        path: &Path,
        records: &mut Vec<DirtySliceRecord>,
        entry_count: &mut usize,
    ) -> anyhow::Result<()> {
        let mut entries = fs::read_dir(path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            account_entry(entry_count, &path)?;
            let kind = entry.file_type().await?;
            anyhow::ensure!(
                !kind.is_symlink() && !kind.is_dir(),
                "packed recovery unexpected nested path"
            );
            if kind.is_file() {
                self.push_packed_recovery_record(path, records).await?;
            }
        }
        Ok(())
    }

    /// The caller holds an admitted recovery driver and its 4 MiB inventory
    /// owner. No normal VFS mutation is admitted until this inventory settles.
    pub(crate) async fn recover_packed_publication(&self) -> anyhow::Result<Vec<DirtySliceRecord>> {
        let dirty = self.root.join("dirty");
        match fs::metadata(&dirty).await {
            Ok(metadata) => {
                anyhow::ensure!(metadata.is_dir(), "packed recovery root is not a directory")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        let mut records = Vec::with_capacity(MAX_RECOVERY_ROWS);
        let mut count = 0usize;
        let mut entries = fs::read_dir(&dirty).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            account_entry(&mut count, &path)?;
            let kind = entry.file_type().await?;
            anyhow::ensure!(!kind.is_symlink(), "packed recovery symlink");
            if kind.is_file() {
                self.push_packed_recovery_record(path, &mut records).await?;
            } else if kind.is_dir() {
                let mut nested = fs::read_dir(&path).await?;
                while let Some(child) = nested.next_entry().await? {
                    let path = child.path();
                    account_entry(&mut count, &path)?;
                    let kind = child.file_type().await?;
                    anyhow::ensure!(!kind.is_symlink(), "packed recovery nested symlink");
                    if kind.is_file() {
                        self.push_packed_recovery_record(path, &mut records).await?;
                    } else if kind.is_dir() {
                        self.scan_packed_recovery_leaf(&path, &mut records, &mut count)
                            .await?;
                    }
                }
            }
        }
        for record in &records {
            self.recoverable_keys.insert(record.key);
        }
        Ok(records)
    }
}
