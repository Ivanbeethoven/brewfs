//! A real SQLite publication with an injected lost reply, not a failed commit.
use std::sync::Arc;

use brewfs::DatabaseMetaStore;
use brewfs::chunk::SliceDesc;
use brewfs::meta::store::{DirEntry, FileAttr, FileType, MetaError, MetaStore, RenameOutcome};

pub struct LostCompactionReply(pub Arc<DatabaseMetaStore>);

#[async_trait::async_trait]
impl MetaStore for LostCompactionReply {
    async fn get_slices(&self, chunk: u64) -> Result<Vec<SliceDesc>, MetaError> {
        self.0.get_slices(chunk).await
    }
    async fn next_id(&self, key: &str) -> Result<i64, MetaError> {
        self.0.next_id(key).await
    }
    async fn record_uncommitted_slice(
        &self,
        slice: u64,
        chunk: u64,
        size: u64,
        operation: &str,
    ) -> Result<i64, MetaError> {
        self.0
            .record_uncommitted_slice(slice, chunk, size, operation)
            .await
    }
    async fn replace_slices_for_compact_with_version(
        &self,
        chunk: u64,
        new: &[SliceDesc],
        delayed: &[u8],
        expected: &[SliceDesc],
    ) -> Result<(), MetaError> {
        self.0
            .replace_slices_for_compact_with_version(chunk, new, delayed, expected)
            .await?;
        Err(MetaError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "injected lost compaction commit reply",
        )))
    }
    async fn confirm_slice_committed(&self, slice: u64) -> Result<(), MetaError> {
        self.0.confirm_slice_committed(slice).await
    }
    async fn delete_uncommitted_slices(&self, slices: &[u64]) -> Result<(), MetaError> {
        self.0.delete_uncommitted_slices(slices).await
    }

    // These required trait methods are outside the compactor's contract.
    // Panic on accidental use rather than supplying fake filesystem results.
    async fn stat(&self, _: i64) -> Result<Option<FileAttr>, MetaError> {
        unreachable!()
    }
    async fn lookup(&self, _: i64, _: &str) -> Result<Option<i64>, MetaError> {
        unreachable!()
    }
    async fn lookup_path(&self, _: &str) -> Result<Option<(i64, FileType)>, MetaError> {
        unreachable!()
    }
    async fn readdir(&self, _: i64) -> Result<Vec<DirEntry>, MetaError> {
        unreachable!()
    }
    async fn mkdir(&self, _: i64, _: String) -> Result<i64, MetaError> {
        unreachable!()
    }
    async fn rmdir(&self, _: i64, _: &str) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn create_file(&self, _: i64, _: String) -> Result<i64, MetaError> {
        unreachable!()
    }
    async fn unlink(&self, _: i64, _: &str) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn rename_with_mode(
        &self,
        _: i64,
        _: &str,
        _: i64,
        _: String,
        _: bool,
    ) -> Result<RenameOutcome, MetaError> {
        unreachable!()
    }
    async fn rename_exchange(&self, _: i64, _: &str, _: i64, _: &str) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn set_file_size(&self, _: i64, _: u64) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn get_names(&self, _: i64) -> Result<Vec<(Option<i64>, String)>, MetaError> {
        unreachable!()
    }
    async fn get_paths(&self, _: i64) -> Result<Vec<String>, MetaError> {
        unreachable!()
    }
    fn root_ino(&self) -> i64 {
        unreachable!()
    }
    async fn initialize(&self) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn get_deleted_files(&self) -> Result<Vec<i64>, MetaError> {
        unreachable!()
    }
    async fn remove_file_metadata(&self, _: i64) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn append_slice(&self, _: u64, _: SliceDesc) -> Result<(), MetaError> {
        unreachable!()
    }
    async fn write(&self, _: i64, _: u64, _: SliceDesc, _: u64) -> Result<(), MetaError> {
        unreachable!()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
