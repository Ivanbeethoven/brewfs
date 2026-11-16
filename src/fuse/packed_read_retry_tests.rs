use super::*;
use crate::meta::store::MetaError;

#[test]
fn packed_read_retry_fuse_errno_preserves_busy_and_stale() {
    for errno in [libc::EBUSY, libc::ESTALE] {
        let error = MetaError::Io(std::io::Error::from_raw_os_error(errno));
        assert_eq!(Errno::from(error), Errno::from(errno));
    }
}

#[test]
fn packed_read_retry_fuse_errno_without_raw_os_error_uses_eio() {
    let error = MetaError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        "error has no raw OS code",
    ));

    assert_eq!(Errno::from(error), Errno::from(libc::EIO));
}
