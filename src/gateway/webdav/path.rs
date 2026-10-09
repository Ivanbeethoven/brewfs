use std::str;

use dav_server::davpath::DavPath;
use dav_server::fs::FsError;

use crate::posix::NAME_MAX;

const SYS_DIR_NAME: &str = ".brewfs.sys";

pub fn to_vfs_path(path: &DavPath) -> Result<String, FsError> {
    let raw = path.as_bytes();
    if raw.is_empty() {
        return Ok("/".to_string());
    }
    let decoded = str::from_utf8(raw).map_err(|_| FsError::Forbidden)?;
    if !decoded.starts_with('/') {
        return Err(FsError::Forbidden);
    }

    let normalized = if decoded.len() > 1 {
        decoded.strip_suffix('/').unwrap_or(decoded)
    } else {
        decoded
    };
    if normalized == "/" {
        return Ok(normalized.to_string());
    }

    let mut components = normalized[1..].split('/');
    if components.clone().next() == Some(SYS_DIR_NAME) {
        return Err(FsError::Forbidden);
    }
    if components.any(|component| {
        component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > NAME_MAX
            || component.contains('\0')
    }) {
        return Err(FsError::Forbidden);
    }

    Ok(normalized.to_string())
}

pub fn ensure_mutable(path: &str) -> Result<(), FsError> {
    if path == "/" {
        Err(FsError::Forbidden)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(path: &str) -> Result<String, FsError> {
        to_vfs_path(&DavPath::new(path).expect("valid DAV path"))
    }

    #[test]
    fn root_is_readable_but_not_mutable() {
        assert_eq!(convert("/"), Ok("/".to_string()));
        assert_eq!(ensure_mutable("/"), Err(FsError::Forbidden));
    }

    #[test]
    fn internal_top_level_parent_maps_to_root() {
        let parent = DavPath::new("/top/").expect("valid DAV path").parent();
        assert!(parent.as_bytes().is_empty());
        assert_eq!(to_vfs_path(&parent), Ok("/".to_string()));
    }

    #[test]
    fn normalizes_collections_and_encoded_utf8() {
        assert_eq!(convert("/docs/"), Ok("/docs".to_string()));
        assert_eq!(convert("/a/./b/../c"), Ok("/a/c".to_string()));
        assert_eq!(convert("/hello%20world"), Ok("/hello world".to_string()));
        assert_eq!(convert("/%E4%B8%AD%E6%96%87/"), Ok("/中文".to_string()));
    }

    #[test]
    fn dav_parser_rejects_encoded_separators_and_nul() {
        assert!(DavPath::new("/a%2Fb").is_err());
        assert!(DavPath::new("/a%00b").is_err());
        assert!(DavPath::new("/../outside").is_err());
    }

    #[test]
    fn rejects_non_utf8_and_internal_namespace() {
        let non_utf8 = DavPath::new("/%FF").expect("DAV path permits non-UTF-8 bytes");
        assert_eq!(to_vfs_path(&non_utf8), Err(FsError::Forbidden));
        assert_eq!(convert("/.brewfs.sys"), Err(FsError::Forbidden));
        assert_eq!(convert("/.brewfs.sys/tmp"), Err(FsError::Forbidden));
        assert_eq!(
            convert("/.brewfs.system"),
            Ok("/.brewfs.system".to_string())
        );
    }

    #[test]
    fn enforces_component_name_limit() {
        let accepted = format!("/{}", "a".repeat(NAME_MAX));
        assert_eq!(convert(&accepted), Ok(accepted));
        let rejected = format!("/{}", "a".repeat(NAME_MAX + 1));
        assert_eq!(convert(&rejected), Err(FsError::Forbidden));
    }
}
