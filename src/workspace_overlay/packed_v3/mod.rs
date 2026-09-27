//! Packed metadata v3 building blocks.
//!
//! This module is deliberately separate from the native-base container kinds.
//! The v3 packed manifest/container objects use their own magic values and are
//! consumed by the workspace lower-layer resolver.

mod catalog;
mod coordinator;
mod group;
mod index;
mod layout;
mod meta;
mod remote;
mod wire;

pub use catalog::RemoteGroupCatalog;
pub use coordinator::{
    CoalescedRange, CoordinatorLimits, FrameReadRequest, GroupReadCoordinator,
    coalesce_frame_ranges, read_coalesced_frames,
};
pub use group::{
    PackedFileInput, PackedFrameDescriptor, PackedFrameInput, PackedGroupContainer,
    PackedGroupDescriptor, PackedGroupInput, frame_directory_body_len, frame_table_body_offset,
    group_container_counts, pack_group_files, parse_frame_descriptor_range, parse_frame_directory,
};
pub use index::{PackedGroupIndexPage, PackedInodeIndexEntry, PackedInodeIndexPage};
pub use layout::{
    AccessProfile, FrameLayoutDecision, LayoutError, SizeClass, SizeClassTable, choose_frame_layout,
};
pub use meta::{GroupMeta, GroupMetaEntry, GroupMetaExtent};
pub use remote::{
    MAX_PACKED_STREAM_RANGE_BYTES, PackedFrameSourceFetcher, RemotePackedObject, read_exact_range,
};
pub use wire::{
    COLD_ATTRIBUTE_MAGIC, GROUP_CONTAINER_MAGIC, GROUP_INDEX_MAGIC, INODE_INDEX_MAGIC,
    MANIFEST_MAGIC, PACKED_FOOTER_LEN, PACKED_FOOTER_MAGIC, PACKED_HEADER_LEN, PackedContainerRef,
    PackedEnvelope, PackedGroupRef, PackedHeader, PackedObjectKind, PackedResult,
    PackedSnapshotManifest, PackedWireError,
};
