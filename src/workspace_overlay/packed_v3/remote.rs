use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use crate::cadapter::client::{ObjectBackend, ObjectClient};
use crate::chunk::read_plan::{ReadGeneration, ReadSource, UnifiedReadSourceFetcher};

use super::group::{
    PackedFrameDescriptor, frame_directory_body_len, frame_table_body_offset,
    group_container_counts, parse_frame_descriptor_range,
};
use super::wire::{
    PACKED_FOOTER_LEN, PACKED_HEADER_LEN, PackedHeader, PackedObjectKind, PackedResult,
    PackedWireError,
};

pub const MAX_PACKED_STREAM_RANGE_BYTES: u64 = 8 * 1024 * 1024 + 64;

/// Consume exactly one bounded range from an object backend.  The backend may
/// yield any chunk sizes, but a short or overlong response is rejected before
/// callers can treat the bytes as a valid frame.
pub async fn read_exact_range<B: ObjectBackend + Clone>(
    client: &ObjectClient<B>,
    key: &str,
    offset: u64,
    length: u64,
) -> PackedResult<Vec<u8>> {
    if length > MAX_PACKED_STREAM_RANGE_BYTES {
        return Err(PackedWireError::LimitExceeded(
            "packed stream range exceeds 8 MiB budget".into(),
        ));
    }
    let expected = usize::try_from(length)
        .map_err(|_| PackedWireError::LimitExceeded("packed stream length exceeds usize".into()))?;
    let mut stream = client
        .get_object_range_stream(key, offset, length)
        .await
        .map_err(|error| PackedWireError::Backend(error.to_string()))?;
    let mut output = Vec::with_capacity(expected);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| PackedWireError::Backend(error.to_string()))?;
        if chunk.len() > expected.saturating_sub(output.len()) {
            return Err(PackedWireError::Invalid(
                "packed stream returned more bytes than requested".into(),
            ));
        }
        output.extend_from_slice(&chunk);
    }
    if output.len() != expected {
        return Err(PackedWireError::Truncated {
            what: "packed streamed range",
            need: expected,
            have: output.len(),
        });
    }
    Ok(output)
}

#[derive(Clone)]
pub struct RemotePackedObject<B: ObjectBackend + Clone> {
    client: ObjectClient<B>,
    object_key: String,
    object_len: u64,
    header: PackedHeader,
}

impl<B: ObjectBackend + Clone> RemotePackedObject<B> {
    pub async fn open(
        client: &ObjectClient<B>,
        object_key: &str,
        object_len: u64,
        expected_kind: PackedObjectKind,
    ) -> PackedResult<Self> {
        if object_key.is_empty() || object_key.len() > 4096 || object_key.contains('\0') {
            return Err(PackedWireError::Invalid(
                "packed object key is empty, too long, or contains NUL".into(),
            ));
        }
        let header_bytes =
            read_exact_range(client, object_key, 0, PACKED_HEADER_LEN as u64).await?;
        let header = PackedHeader::parse(&header_bytes)?;
        if header.kind != expected_kind {
            return Err(PackedWireError::Invalid(
                "packed object kind does not match the requested reader".into(),
            ));
        }
        if header.object_len != object_len {
            return Err(PackedWireError::Invalid(
                "packed object length disagrees with manifest".into(),
            ));
        }
        Ok(Self {
            client: client.clone(),
            object_key: object_key.to_owned(),
            object_len,
            header,
        })
    }

    pub fn header(&self) -> &PackedHeader {
        &self.header
    }

    pub fn object_len(&self) -> u64 {
        self.object_len
    }

    pub async fn read_range(&self, offset: u64, length: u64) -> PackedResult<Vec<u8>> {
        let footer_offset = self
            .object_len
            .checked_sub(PACKED_FOOTER_LEN as u64)
            .ok_or_else(|| {
                PackedWireError::Invalid("packed object is shorter than footer".into())
            })?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| PackedWireError::LimitExceeded("packed range overflows".into()))?;
        if offset < PACKED_HEADER_LEN as u64 || end > footer_offset {
            return Err(PackedWireError::Invalid(
                "packed range is outside the object body".into(),
            ));
        }
        read_exact_range(&self.client, &self.object_key, offset, length).await
    }

    pub async fn read_frame(&self, frame: &PackedFrameDescriptor) -> PackedResult<Bytes> {
        let bytes = self
            .read_range(frame.object_offset, u64::from(frame.stored_len))
            .await?;
        let digest: [u8; 16] = Sha256::digest(&bytes)[..16]
            .try_into()
            .expect("sha256 prefix has 16 bytes");
        if digest != frame.frame_digest {
            return Err(PackedWireError::Invalid(
                "packed frame digest mismatch".into(),
            ));
        }
        Ok(Bytes::from(bytes))
    }

    /// Read the complete frame directory using bounded ranges.  The first
    /// request fetches only the 24-byte body prefix; descriptor records are
    /// then fetched in chunks no larger than the streaming range budget.  No
    /// metadata, frame-list or frame payload bytes are touched.
    pub async fn read_frame_directory(&self) -> PackedResult<Vec<PackedFrameDescriptor>> {
        let prefix = self.read_range(PACKED_HEADER_LEN as u64, 24).await?;
        let required_len = frame_directory_body_len(&prefix)?;
        if required_len > usize::try_from(self.header.body_stored_len).unwrap_or(usize::MAX) {
            return Err(PackedWireError::Truncated {
                what: "packed group frame directory",
                need: required_len,
                have: usize::try_from(self.header.body_stored_len).unwrap_or(usize::MAX),
            });
        }
        let (_, frame_count) = group_container_counts(&prefix)?;
        let table_offset = frame_table_body_offset(&prefix)?;
        let records_per_range = usize::try_from(MAX_PACKED_STREAM_RANGE_BYTES)
            .unwrap_or(usize::MAX)
            .saturating_div(super::group::FRAME_RECORD_LEN);
        if records_per_range == 0 {
            return Err(PackedWireError::LimitExceeded(
                "packed frame directory range budget is too small".into(),
            ));
        }
        let mut frames = Vec::with_capacity(frame_count as usize);
        let mut first = 0u32;
        while first < frame_count {
            let count = usize::try_from(frame_count - first)
                .unwrap_or(usize::MAX)
                .min(records_per_range);
            let length = count
                .checked_mul(super::group::FRAME_RECORD_LEN)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory range overflows".into())
                })?;
            let body_offset = table_offset
                .checked_add(
                    usize::try_from(first)
                        .ok()
                        .and_then(|value| value.checked_mul(super::group::FRAME_RECORD_LEN))
                        .ok_or_else(|| {
                            PackedWireError::LimitExceeded(
                                "frame directory offset overflows".into(),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory offset overflows".into())
                })?;
            let object_offset = (PACKED_HEADER_LEN as u64)
                .checked_add(body_offset as u64)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory object offset overflows".into())
                })?;
            let bytes = self.read_range(object_offset, length as u64).await?;
            frames.extend(parse_frame_descriptor_range(
                &bytes,
                self.object_len,
                first,
            )?);
            first = first
                .checked_add(u32::try_from(count).map_err(|_| {
                    PackedWireError::LimitExceeded("frame directory count exceeds u32".into())
                })?)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory ordinal overflows".into())
                })?;
        }
        validate_descriptor_order(&frames)?;
        Ok(frames)
    }

    /// Read only the requested frame descriptors.  Ordinals are sorted and
    /// contiguous runs are merged, with every object-store range bounded by
    /// `MAX_PACKED_STREAM_RANGE_BYTES`.  This avoids loading a million-frame
    /// table when a single GroupMeta entry references only a few frames.
    pub async fn read_frame_descriptors(
        &self,
        ordinals: impl IntoIterator<Item = u32>,
    ) -> PackedResult<Vec<PackedFrameDescriptor>> {
        let mut ordinals: Vec<u32> = ordinals.into_iter().collect();
        if ordinals.is_empty() {
            return Ok(Vec::new());
        }
        ordinals.sort_unstable();
        ordinals.dedup();
        let prefix = self.read_range(PACKED_HEADER_LEN as u64, 24).await?;
        let (_, frame_count) = group_container_counts(&prefix)?;
        if ordinals.iter().any(|ordinal| *ordinal >= frame_count) {
            return Err(PackedWireError::Invalid(
                "requested frame ordinal exceeds container frame count".into(),
            ));
        }
        let table_offset = frame_table_body_offset(&prefix)?;
        let max_records = usize::try_from(MAX_PACKED_STREAM_RANGE_BYTES)
            .unwrap_or(usize::MAX)
            .saturating_div(super::group::FRAME_RECORD_LEN)
            .max(1);
        let mut output = Vec::with_capacity(ordinals.len());
        let mut index = 0usize;
        while index < ordinals.len() {
            let start = ordinals[index];
            let mut end = start.checked_add(1).ok_or_else(|| {
                PackedWireError::LimitExceeded("frame ordinal range overflows".into())
            })?;
            let mut next = index + 1;
            while next < ordinals.len()
                && ordinals[next] == end
                && usize::try_from(end - start).unwrap_or(usize::MAX) < max_records
            {
                end = end.checked_add(1).ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame ordinal range overflows".into())
                })?;
                next += 1;
            }
            let count = usize::try_from(end - start).map_err(|_| {
                PackedWireError::LimitExceeded("frame descriptor count exceeds usize".into())
            })?;
            let body_offset = table_offset
                .checked_add(
                    usize::try_from(start)
                        .ok()
                        .and_then(|value| value.checked_mul(super::group::FRAME_RECORD_LEN))
                        .ok_or_else(|| {
                            PackedWireError::LimitExceeded(
                                "frame directory offset overflows".into(),
                            )
                        })?,
                )
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory offset overflows".into())
                })?;
            let object_offset = (PACKED_HEADER_LEN as u64)
                .checked_add(body_offset as u64)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory object offset overflows".into())
                })?;
            let length = count
                .checked_mul(super::group::FRAME_RECORD_LEN)
                .ok_or_else(|| {
                    PackedWireError::LimitExceeded("frame directory range overflows".into())
                })?;
            let bytes = self.read_range(object_offset, length as u64).await?;
            output.extend(parse_frame_descriptor_range(
                &bytes,
                self.object_len,
                start,
            )?);
            index = next;
        }
        validate_descriptor_order(&output)?;
        Ok(output)
    }
}

fn validate_descriptor_order(frames: &[PackedFrameDescriptor]) -> PackedResult<()> {
    let mut previous = None;
    for frame in frames {
        let end = frame
            .object_offset
            .checked_add(u64::from(frame.stored_len))
            .ok_or_else(|| PackedWireError::LimitExceeded("frame range overflows".into()))?;
        if let Some(previous_end) = previous {
            if frame.object_offset < previous_end {
                return Err(PackedWireError::Invalid(
                    "frame payload ranges overlap or are not canonical".into(),
                ));
            }
        }
        previous = Some(end);
    }
    Ok(())
}

/// Adapter that lets the common unified read executor consume packed frames.
/// The map is populated from the pinned manifest; no metadata service is
/// consulted while a frame is being delivered.
#[derive(Clone)]
pub struct PackedFrameSourceFetcher<B: ObjectBackend + Clone> {
    objects: Arc<HashMap<u32, RemotePackedObject<B>>>,
}

impl<B: ObjectBackend + Clone> PackedFrameSourceFetcher<B> {
    pub fn new(objects: HashMap<u32, RemotePackedObject<B>>) -> Self {
        Self {
            objects: Arc::new(objects),
        }
    }

    pub fn from_object(object: RemotePackedObject<B>, container_ordinal: u32) -> Self {
        let mut objects = HashMap::new();
        objects.insert(container_ordinal, object);
        Self::new(objects)
    }
}

#[async_trait]
impl<B: ObjectBackend + Clone + 'static> UnifiedReadSourceFetcher for PackedFrameSourceFetcher<B> {
    async fn read_source(&self, source: &ReadSource, output: &mut [u8]) -> anyhow::Result<()> {
        match source {
            ReadSource::Hole => {
                output.fill(0);
                Ok(())
            }
            ReadSource::PackedFrame {
                container_ordinal,
                frame_ordinal,
                object_offset,
                stored_len,
                raw_offset,
                raw_len,
                size_class,
                codec,
                frame_digest,
                ..
            } => {
                if *codec != 0 || *stored_len != *raw_len {
                    anyhow::bail!("unsupported packed frame codec or lengths")
                }
                let object = self.objects.get(container_ordinal).ok_or_else(|| {
                    anyhow::anyhow!("packed frame container {container_ordinal} is not pinned")
                })?;
                let descriptor = PackedFrameDescriptor {
                    frame_ordinal: *frame_ordinal,
                    object_offset: *object_offset,
                    stored_len: *stored_len,
                    raw_len: *raw_len,
                    first_file_slot: 0,
                    last_file_slot: 0,
                    size_class: super::layout::SizeClass::from_u8(*size_class)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?,
                    codec: *codec,
                    frame_digest: *frame_digest,
                };
                let frame = object
                    .read_frame(&descriptor)
                    .await
                    .map_err(|error| anyhow::anyhow!("packed frame read failed: {error}"))?;
                let start = usize::try_from(*raw_offset)
                    .map_err(|_| anyhow::anyhow!("packed raw offset exceeds usize"))?;
                let end = start
                    .checked_add(output.len())
                    .ok_or_else(|| anyhow::anyhow!("packed output range overflows"))?;
                if end > frame.len() {
                    anyhow::bail!("packed source range exceeds frame raw length")
                }
                output.copy_from_slice(&frame[start..end]);
                Ok(())
            }
            ReadSource::UpperBlock { .. } | ReadSource::LegacySlice { .. } => {
                anyhow::bail!("packed frame fetcher cannot read mutable or legacy sources")
            }
        }
    }

    async fn ensure_generation(&self, _generation: ReadGeneration) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadapter::localfs::LocalFsBackend;
    use crate::workspace_overlay::packed_v3::{
        AccessProfile, GroupMeta, GroupMetaEntry, GroupMetaExtent, PackedFrameInput,
        PackedGroupContainer, PackedGroupInput, SizeClass,
    };
    use tempfile::tempdir;

    #[tokio::test]
    async fn remote_reader_consumes_a_bounded_stream_range() {
        let temp = tempdir().unwrap();
        let backend = LocalFsBackend::new(temp.path());
        let client = ObjectClient::new(backend.clone());
        let object = PackedGroupContainer::build(
            7,
            AccessProfile::RandomSmallFile,
            vec![PackedGroupInput {
                group_id: 1,
                parent_dir_key: [0; 32],
                metadata: GroupMeta::new(vec![GroupMetaEntry {
                    name: b"payload".to_vec(),
                    inode: 1,
                    kind: 1,
                    mode: 0o100644,
                    size: 7,
                    flags: 0,
                    extents: vec![GroupMetaExtent {
                        file_offset: 0,
                        logical_len: 7,
                        frame_ordinal: 0,
                        raw_offset: 0,
                        raw_len: 7,
                    }],
                }])
                .unwrap()
                .encode()
                .unwrap(),
                frame_ordinals: vec![0],
                entry_count: 1,
                file_count: 1,
                layout_profile: AccessProfile::RandomSmallFile,
            }],
            vec![PackedFrameInput {
                raw: b"payload".to_vec(),
                size_class: SizeClass::Tiny,
                codec: 0,
                first_file_slot: 0,
                last_file_slot: 0,
            }],
        )
        .unwrap();
        client.put_object("group", &object).await.unwrap();
        let opened = PackedGroupContainer::open(object).unwrap();
        let remote = RemotePackedObject::open(
            &client,
            "group",
            opened.object_len(),
            PackedObjectKind::GroupContainer,
        )
        .await
        .unwrap();
        let frame = remote.read_frame(&opened.frames()[0]).await.unwrap();
        assert_eq!(frame.as_ref(), b"payload");
        let directory = remote.read_frame_directory().await.unwrap();
        assert_eq!(directory, opened.frames());
        let selected = remote.read_frame_descriptors([0]).await.unwrap();
        assert_eq!(selected, opened.frames());
    }
}
