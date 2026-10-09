use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::cadapter::client::{ObjectBackend, ObjectClient};
use crate::cadapter::read_observer::{FailureClass, ReadClass, ReadWork};
use crate::native_base::wire::container::ObjectKind;
use crate::native_base::wire::refs::ObjectRef;
use crate::native_base::write::receipts::ObjectSink;

/// Native immutable-object repository backed by BrewFS' production object
/// client. Every PUT is atomic create-only and is followed by exact readback
/// before the write pipeline may produce a receipt.
pub struct BackendObjectRepository<B: ObjectBackend> {
    client: ObjectClient<B>,
}

impl<B: ObjectBackend> BackendObjectRepository<B> {
    pub fn new(client: ObjectClient<B>) -> Self {
        Self { client }
    }

    fn read_class(object: &ObjectRef) -> anyhow::Result<ReadClass> {
        match object.kind {
            kind if kind == ObjectKind::DataPack.as_u8() || kind == 6 => {
                Ok(ReadClass::NativePayload)
            }
            kind if kind == ObjectKind::DataSeal.as_u8()
                || kind == ObjectKind::SnapshotManifest.as_u8()
                || kind == ObjectKind::PagedInventory.as_u8() =>
            {
                Ok(ReadClass::NativeIndex)
            }
            kind if kind == ObjectKind::FrozenMetadata.as_u8() => Ok(ReadClass::NativeAttributes),
            _ => anyhow::bail!("unsupported native object kind"),
        }
    }
    async fn read_verified(&self, object: &ObjectRef) -> anyhow::Result<Option<Vec<u8>>> {
        let class = Self::read_class(object)?;
        self.client
            .typed_full(
                class,
                Self::key(object)?,
                Some(object.object_len),
                object.object_len,
                |bytes| {
                    let _authentication = self
                        .client
                        .measure_read_work(class, ReadWork::Authentication);
                    Self::verify(object, &bytes)
                        .map_err(|error| (FailureClass::Authentication, error))?;
                    Ok(bytes)
                },
            )
            .await
    }

    fn key(object: &ObjectRef) -> anyhow::Result<&str> {
        std::str::from_utf8(&object.key)
            .map_err(|_| anyhow::anyhow!("native object key is not UTF-8"))
    }

    fn verify(object: &ObjectRef, bytes: &[u8]) -> anyhow::Result<()> {
        if bytes.len() as u64 != object.object_len {
            anyhow::bail!(
                "native object length mismatch: expected {}, got {}",
                object.object_len,
                bytes.len()
            )
        }
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if digest != object.full_hash {
            anyhow::bail!("native object digest mismatch")
        }
        Ok(())
    }
}

#[async_trait]
impl<B> ObjectSink for BackendObjectRepository<B>
where
    B: ObjectBackend + Send + Sync,
{
    async fn put(&self, object: &ObjectRef, bytes: &[u8]) -> anyhow::Result<()> {
        Self::verify(object, bytes)?;
        let key = Self::key(object)?;
        if let Err(error) = self.client.put_object_create_only(key, bytes).await {
            let existing = self.read_verified(object).await?.ok_or(error)?;
            if existing != bytes {
                anyhow::bail!("create-only conflict for native object {key}")
            }
        }
        let readback = self
            .read_verified(object)
            .await?
            .ok_or_else(|| anyhow::anyhow!("native object disappeared after PUT: {key}"))?;
        if readback != bytes {
            anyhow::bail!("native object exact readback differs after PUT: {key}")
        }
        Ok(())
    }

    async fn get(&self, object: &ObjectRef) -> anyhow::Result<Vec<u8>> {
        let key = Self::key(object)?;
        let bytes = self
            .read_verified(object)
            .await?
            .ok_or_else(|| anyhow::anyhow!("native object is missing: {key}"))?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadapter::localfs::LocalFsBackend;
    use crate::cadapter::read_observer::{
        Engine, Ledger, Origin, Phase, ReadContext, ReadObserver,
    };

    fn reference(key: &str, bytes: &[u8], kind: u8) -> ObjectRef {
        ObjectRef {
            object_id: [1; 16],
            kind,
            object_len: bytes.len() as u64,
            full_hash: Sha256::digest(bytes).into(),
            key: key.as_bytes().to_vec(),
        }
    }

    #[tokio::test]
    async fn repository_real_full_reads_classify_loose_metadata_and_reject_tamper() {
        let directory = tempfile::tempdir().unwrap();
        let backend = LocalFsBackend::new(directory.path());
        let observer = std::sync::Arc::new(ReadObserver::default());
        let client = ObjectClient::new(backend.clone()).with_read_observer(
            observer.clone(),
            Engine::Native,
            Phase::Runtime,
            Origin::Demand,
        );
        let repository = BackendObjectRepository::new(client);
        for (key, kind, class) in [
            ("opaque-one", 6, ReadClass::NativePayload),
            (
                "looks-like-data",
                ObjectKind::FrozenMetadata.as_u8(),
                ReadClass::NativeAttributes,
            ),
            (
                "looks-like-attributes",
                ObjectKind::DataSeal.as_u8(),
                ReadClass::NativeIndex,
            ),
        ] {
            let bytes = b"verified native body";
            backend.put_object(key, bytes).await.unwrap();
            let object = reference(key, bytes, kind);
            assert_eq!(repository.get(&object).await.unwrap(), bytes);
            backend
                .put_object(key, b"replaced native body")
                .await
                .unwrap();
            assert!(repository.get(&object).await.is_err());
            let context = ReadContext {
                engine: Engine::Native,
                phase: Phase::Runtime,
                class,
                origin: Origin::Demand,
            };
            let snapshot = observer.snapshot();
            let body = &snapshot.rows[&(Ledger::BackendBody, context)];
            assert!(body.received > 0);
            assert!(body.conserved());
            let validated = &snapshot.rows[&(Ledger::ValidatedFetch, context)];
            assert_eq!((validated.success, validated.failed), (1, 1));
            assert!(validated.conserved());
        }
        let object = reference("never-fetch", b"irrelevant", 255);
        let before = observer.snapshot().rows;
        assert!(repository.get(&object).await.is_err());
        assert_eq!(observer.snapshot().rows.len(), before.len());
    }
}
