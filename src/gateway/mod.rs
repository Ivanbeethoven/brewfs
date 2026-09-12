//! Protocol gateways for BrewFS.
//!
//! Gateways expose a BrewFS volume over network protocols (S3, WebDAV, NFS)
//! without going through the kernel FUSE mount. They sit beside `src/fuse/`
//! on the same core stack (VFS over meta + object storage backends).
//!
//! See `doc/protocols/` for the design specs and the milestone roadmap.

/// Hidden system directory that holds gateway-internal state
/// (multipart uploads, staging files, distributed locks).
///
/// All protocol listing operations must filter this directory out at the
/// volume root. See `doc/protocols/README.md` §3.1.
pub const SYS_DIR: &str = "/.brewfs.sys";

#[cfg(feature = "gateway-s3")]
/// Returns the S3 gateway subsystem directory under [`SYS_DIR`].
pub fn s3_sys_dir() -> String {
    format!("{SYS_DIR}/s3")
}

#[cfg(feature = "gateway-webdav")]
pub fn webdav_sys_dir() -> String {
    format!("{SYS_DIR}/webdav")
}

#[cfg(feature = "gateway-s3")]
pub mod s3;

#[cfg(feature = "gateway-webdav")]
pub mod webdav;
