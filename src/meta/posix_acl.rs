//! Linux POSIX ACL xattr encoding and shared inode access policy.

pub const ACCESS_XATTR: &[u8] = b"system.posix_acl_access";
pub const DEFAULT_XATTR: &[u8] = b"system.posix_acl_default";
const UNDEFINED_ID: u32 = u32::MAX;
const MAX_BYTES: usize = 65536;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PosixAcl {
    entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Entry {
    tag: u16,
    permissions: u16,
    id: u32,
}

impl PosixAcl {
    /// Require the canonical Linux order and exact singleton/mask rules.
    /// Missing/unknown/duplicate entries are corruption, never a mode fallback.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 28
            || bytes.len() > MAX_BYTES
            || !(bytes.len() - 4).is_multiple_of(8)
            || bytes[..4] != 2u32.to_le_bytes()
        {
            return Err("invalid POSIX ACL version/length");
        }
        let mut entries = Vec::with_capacity((bytes.len() - 4) / 8);
        let mut last_tag = 0;
        let mut last_id = None;
        let mut named = false;
        let mut mask = false;
        let mut user_obj = false;
        let mut group_obj = false;
        let mut other = false;
        for raw in bytes[4..].as_chunks::<8>().0 {
            let entry = Entry {
                tag: u16::from_le_bytes(raw[..2].try_into().unwrap()),
                permissions: u16::from_le_bytes(raw[2..4].try_into().unwrap()),
                id: u32::from_le_bytes(raw[4..].try_into().unwrap()),
            };
            if !matches!(entry.tag, 1 | 2 | 4 | 8 | 16 | 32)
                || entry.permissions > 7
                || entry.tag < last_tag
            {
                return Err("invalid POSIX ACL tag/permissions/order");
            }
            if matches!(entry.tag, 2 | 8) {
                if entry.id == UNDEFINED_ID
                    || (last_tag == entry.tag && last_id.is_some_and(|id| id >= entry.id))
                {
                    return Err("invalid POSIX ACL named identity/order");
                }
                named = true;
            } else {
                if entry.id != UNDEFINED_ID || last_tag == entry.tag {
                    return Err("invalid POSIX ACL singleton identity/duplicate");
                }
                match entry.tag {
                    1 => user_obj = true,
                    4 => group_obj = true,
                    16 => mask = true,
                    32 => other = true,
                    _ => unreachable!(),
                }
            }
            last_tag = entry.tag;
            last_id = Some(entry.id);
            entries.push(entry);
        }
        if !user_obj || !group_obj || !other || (named && !mask) {
            return Err("incomplete POSIX ACL base/mask entries");
        }
        Ok(Self { entries })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(4 + 8 * self.entries.len());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        for entry in &self.entries {
            bytes.extend_from_slice(&entry.tag.to_le_bytes());
            bytes.extend_from_slice(&entry.permissions.to_le_bytes());
            bytes.extend_from_slice(&entry.id.to_le_bytes());
        }
        bytes
    }

    /// An access ACL with only the three base entries is represented by mode
    /// bits alone. A mask or named entry must remain stored even if redundant.
    pub fn is_extended(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry.tag, 2 | 8 | 16))
    }

    fn permission(&self, tag: u16) -> u32 {
        self.entries
            .iter()
            .find(|entry| entry.tag == tag)
            .map_or(0, |entry| u32::from(entry.permissions))
    }

    pub fn mode_bits(&self) -> u32 {
        let group = if self.entries.iter().any(|entry| entry.tag == 16) {
            self.permission(16)
        } else {
            self.permission(4)
        };
        (self.permission(1) << 6) | (group << 3) | self.permission(32)
    }

    /// Owner and named-user decisions do not depend on process group state.
    pub fn user_access_mode(&self, owner: u32, uid: u32) -> Option<u32> {
        if uid == owner {
            return Some(self.permission(1));
        }
        // Linux acl_permission_check skips check_acl when the inode's group
        // mode bits are zero. That branch needs owning-group membership even
        // for a named user, so it cannot decide from uid alone.
        if self.mode_bits() & 0o070 == 0 {
            return None;
        }
        let mask = self
            .entries
            .iter()
            .find(|entry| entry.tag == 16)
            .map_or(7, |entry| u32::from(entry.permissions));
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.tag == 2 && entry.id == uid)
        {
            return Some(u32::from(entry.permissions) & mask);
        }
        None
    }

    /// Linux requires one matching group entry to grant the entire requested
    /// operation. Individual read/write grants from different groups must not
    /// be combined to authorize O_RDWR or R_OK|X_OK.
    pub fn allows_access(
        &self,
        owner: u32,
        owning_group: u32,
        uid: u32,
        groups: &[u32],
        requested: u32,
    ) -> bool {
        let allows = |mode: u32| mode & requested == requested;
        if let Some(mode) = self.user_access_mode(owner, uid) {
            return allows(mode);
        }
        if self.mode_bits() & 0o070 == 0 {
            return allows(if groups.contains(&owning_group) {
                0
            } else {
                self.permission(32)
            });
        }
        let mask = self
            .entries
            .iter()
            .find(|entry| entry.tag == 16)
            .map_or(7, |entry| u32::from(entry.permissions));
        let mut matched = false;
        for entry in &self.entries {
            if (entry.tag == 4 && groups.contains(&owning_group))
                || (entry.tag == 8 && groups.contains(&entry.id))
            {
                matched = true;
                if allows(u32::from(entry.permissions) & mask) {
                    return true;
                }
            }
        }
        !matched && allows(self.permission(32))
    }

    /// POSIX create: parent default ACL replaces umask, then the requested
    /// mode intersects user_obj, mask/group_obj and other. Directories retain
    /// an unchanged copy of the parent's default ACL for their descendants.
    pub fn inherited_access(&self, requested_mode: u32) -> Self {
        let mut acl = self.clone();
        let has_mask = acl.entries.iter().any(|entry| entry.tag == 16);
        for entry in &mut acl.entries {
            let requested = match entry.tag {
                1 => Some((requested_mode >> 6) & 7),
                16 => Some((requested_mode >> 3) & 7),
                4 if !has_mask => Some((requested_mode >> 3) & 7),
                32 => Some(requested_mode & 7),
                _ => None,
            };
            if let Some(requested) = requested {
                entry.permissions &= requested as u16;
            }
        }
        acl
    }

    pub fn chmod(&self, mode: u32) -> Self {
        let mut acl = self.clone();
        let has_mask = acl.entries.iter().any(|entry| entry.tag == 16);
        for entry in &mut acl.entries {
            let permission = match entry.tag {
                1 => Some((mode >> 6) & 7),
                16 => Some((mode >> 3) & 7),
                4 if !has_mask => Some((mode >> 3) & 7),
                32 => Some(mode & 7),
                _ => None,
            };
            if let Some(permission) = permission {
                entry.permissions = permission as u16;
            }
        }
        acl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn acl(rows: &[(u16, u16, u32)]) -> Vec<u8> {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for (tag, permission, id) in rows {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&permission.to_le_bytes());
            bytes.extend_from_slice(&id.to_le_bytes());
        }
        bytes
    }
    fn extended() -> PosixAcl {
        PosixAcl::decode(&acl(&[
            (1, 6, UNDEFINED_ID),
            (2, 0, 1234),
            (2, 7, 2000),
            (4, 0, UNDEFINED_ID),
            (8, 2, 3000),
            (8, 5, 4000),
            (16, 6, UNDEFINED_ID),
            (32, 7, UNDEFINED_ID),
        ]))
        .unwrap()
    }
    fn access_mode(acl: &PosixAcl, owner: u32, owning_group: u32, uid: u32, groups: &[u32]) -> u32 {
        [1, 2, 4]
            .into_iter()
            .filter(|bit| acl.allows_access(owner, owning_group, uid, groups, *bit))
            .sum()
    }
    #[test]
    fn named_user_denial_and_group_union_never_fall_back_to_other() {
        let acl = extended();
        assert_eq!(access_mode(&acl, 1000, 2000, 1000, &[4000]), 6);
        assert_eq!(access_mode(&acl, 1000, 2000, 1234, &[4000]), 0);
        assert_eq!(access_mode(&acl, 1000, 2000, 2000, &[4000]), 6);
        assert_eq!(access_mode(&acl, 1000, 2000, 9000, &[5000, 3000, 4000]), 6);
        assert_eq!(access_mode(&acl, 1000, 2000, 9000, &[2000]), 0);
        assert_eq!(access_mode(&acl, 1000, 2000, 9000, &[5000]), 7);
        assert_eq!(PosixAcl::decode(&acl.encode()).unwrap(), acl);
    }
    #[test]
    fn default_inheritance_and_chmod_preserve_named_entries_and_update_class_mask() {
        let parent = extended();
        let child = parent.inherited_access(0o640);
        assert_eq!(child.mode_bits(), 0o640);
        assert_eq!(access_mode(&child, 1000, 2000, 2000, &[]), 4);
        assert_eq!(access_mode(&parent, 1000, 2000, 2000, &[]), 6);
        let changed = child.chmod(0o751);
        assert_eq!(changed.mode_bits(), 0o751);
        assert_eq!(access_mode(&changed, 1000, 2000, 2000, &[]), 5);
        assert_eq!(access_mode(&changed, 1000, 2000, 1234, &[4000]), 0);
    }

    #[test]
    fn combined_operation_requires_one_group_to_grant_all_requested_permissions() {
        let acl = PosixAcl::decode(&acl(&[
            (1, 7, UNDEFINED_ID),
            (4, 0, UNDEFINED_ID),
            (8, 4, 3000),
            (8, 2, 4000),
            (16, 6, UNDEFINED_ID),
            (32, 7, UNDEFINED_ID),
        ]))
        .unwrap();
        assert!(acl.allows_access(1000, 2000, 9000, &[3000, 4000], 4));
        assert!(acl.allows_access(1000, 2000, 9000, &[3000, 4000], 2));
        assert!(!acl.allows_access(1000, 2000, 9000, &[3000, 4000], 6));
        assert!(!acl.allows_access(1000, 2000, 9000, &[2000], 4));
        assert!(acl.allows_access(1000, 2000, 9000, &[5000], 6));
    }

    #[test]
    fn linux_zero_group_mode_uses_owning_group_or_other_despite_named_entries() {
        let acl = PosixAcl::decode(&acl(&[
            (1, 6, UNDEFINED_ID),
            (2, 7, 3000),
            (4, 0, UNDEFINED_ID),
            (8, 7, 4000),
            (16, 0, UNDEFINED_ID),
            (32, 4, UNDEFINED_ID),
        ]))
        .unwrap();
        assert_eq!(acl.mode_bits(), 0o604);
        assert_eq!(acl.user_access_mode(1000, 3000), None);
        assert!(acl.allows_access(1000, 2000, 3000, &[4000], 4));
        assert!(!acl.allows_access(1000, 2000, 3000, &[2000, 4000], 4));
        assert!(!acl.allows_access(1000, 2000, 3000, &[4000], 2));
        assert!(acl.allows_access(1000, 2000, 1000, &[2000], 6));
    }
    #[test]
    fn malformed_unknown_duplicate_unordered_and_missing_acl_entries_are_rejected() {
        let valid = extended().encode();
        for end in 0..valid.len() {
            assert!(PosixAcl::decode(&valid[..end]).is_err());
        }
        for rows in [
            vec![
                (1, 6, UNDEFINED_ID),
                (2, 4, 1000),
                (4, 0, UNDEFINED_ID),
                (32, 0, UNDEFINED_ID),
            ],
            vec![(1, 6, UNDEFINED_ID), (4, 0, UNDEFINED_ID)],
            vec![
                (1, 6, UNDEFINED_ID),
                (1, 6, UNDEFINED_ID),
                (4, 0, UNDEFINED_ID),
                (32, 0, UNDEFINED_ID),
            ],
            vec![(1, 6, 0), (4, 0, UNDEFINED_ID), (32, 0, UNDEFINED_ID)],
            vec![
                (1, 8, UNDEFINED_ID),
                (4, 0, UNDEFINED_ID),
                (32, 0, UNDEFINED_ID),
            ],
            vec![
                (1, 6, UNDEFINED_ID),
                (2, 4, 2000),
                (2, 4, 1000),
                (4, 0, UNDEFINED_ID),
                (16, 4, UNDEFINED_ID),
                (32, 0, UNDEFINED_ID),
            ],
        ] {
            assert!(PosixAcl::decode(&acl(&rows)).is_err());
        }
        let mut version = valid.clone();
        version[0] = 3;
        assert!(PosixAcl::decode(&version).is_err());
        let mut trailing = valid;
        trailing.push(0);
        assert!(PosixAcl::decode(&trailing).is_err());
    }
}

/// Control ACL access entries use the same user/group precedence in metadata
/// and FUSE. None means that no applicable access entry exists, so mode applies.
pub(crate) fn control_acl_access_mode(
    entries: &[crate::control::protocol::ControlAclEntry],
    owner_uid: u32,
    owner_gid: u32,
    uid: u32,
    groups: &[u32],
) -> Option<u32> {
    let find = |tag: &str, id: Option<u32>| {
        entries
            .iter()
            .find(|entry| entry.scope == "access" && entry.tag == tag && entry.id == id)
            .and_then(|entry| control_acl_perm_bits(&entry.perm))
    };
    let masked = |mode: u32| mode & find("mask", None).unwrap_or(7);
    if uid == owner_uid {
        return find("user_obj", None);
    }
    if let Some(mode) = find("user", Some(uid)) {
        return Some(masked(mode));
    }
    let mut matched = None;
    if groups.contains(&owner_gid) {
        matched = find("group_obj", None);
    }
    for gid in groups {
        if let Some(mode) = find("group", Some(*gid)) {
            matched = Some(matched.unwrap_or(0) | mode);
        }
    }
    matched.map(masked).or_else(|| find("other", None))
}

/// Evaluate a control ACL for one operation mask.
///
/// POSIX ACL group-class entries are alternatives: one matching group entry
/// must grant the complete requested operation.  Combining read permission
/// from one group with write permission from another would incorrectly allow
/// `O_RDWR`, so callers that have a concrete operation must use this helper
/// instead of materialising an aggregate mode first.
pub(crate) fn control_acl_allows_access(
    entries: &[crate::control::protocol::ControlAclEntry],
    owner_uid: u32,
    owner_gid: u32,
    uid: u32,
    groups: &[u32],
    requested: u32,
) -> Option<bool> {
    let find = |tag: &str, id: Option<u32>| {
        entries
            .iter()
            .find(|entry| entry.scope == "access" && entry.tag == tag && entry.id == id)
            .and_then(|entry| control_acl_perm_bits(&entry.perm))
    };
    let masked = |mode: u32| mode & find("mask", None).unwrap_or(7);
    if uid == owner_uid {
        return find("user_obj", None).map(|mode| mode & requested == requested);
    }
    // A matching named-user entry is terminal, even when it denies access.
    if let Some(mode) = find("user", Some(uid)) {
        return Some(masked(mode) & requested == requested);
    }

    let mut matched_group = false;
    if groups.contains(&owner_gid) {
        matched_group = true;
        if masked(find("group_obj", None).unwrap_or(0)) & requested == requested {
            return Some(true);
        }
    }
    for gid in groups {
        if let Some(mode) = find("group", Some(*gid)) {
            matched_group = true;
            if masked(mode) & requested == requested {
                return Some(true);
            }
        }
    }
    if matched_group {
        return Some(false);
    }
    find("other", None).map(|mode| mode & requested == requested)
}

fn control_acl_perm_bits(perm: &str) -> Option<u32> {
    if perm.len() != 3 {
        return None;
    }
    let mut mode = 0;
    for (index, ch) in perm.chars().enumerate() {
        match (index, ch) {
            (0, 'r') => mode |= 4,
            (1, 'w') => mode |= 2,
            (2, 'x') => mode |= 1,
            (_, '-') => {}
            _ => return None,
        }
    }
    Some(mode)
}

#[cfg(test)]
mod control_policy_tests {
    use super::{control_acl_access_mode, control_acl_allows_access};
    use crate::control::protocol::ControlAclEntry;
    #[test]
    fn supplementary_group_match_is_masked_and_other_cannot_bypass_it() {
        let entry = |tag: &str, id, perm: &str| ControlAclEntry {
            scope: "access".into(),
            tag: tag.into(),
            id,
            perm: perm.into(),
        };
        let mut entries = vec![
            entry("user_obj", None, "rwx"),
            entry("group_obj", None, "---"),
            entry("group", Some(4444), "rwx"),
            entry("mask", None, "r--"),
            entry("other", None, "rwx"),
        ];
        assert_eq!(
            control_acl_access_mode(&entries, 1000, 2000, 1234, &[3000, 4444]),
            Some(4)
        );
        assert_eq!(
            control_acl_access_mode(&entries, 1000, 2000, 1234, &[2000, 3000]),
            Some(0)
        );
        entries.push(entry("user", Some(1234), "---"));
        assert_eq!(
            control_acl_access_mode(&entries, 1000, 2000, 1234, &[4444]),
            Some(0)
        );
    }

    #[test]
    fn supplementary_group_entries_do_not_union_for_one_operation() {
        let entry = |tag: &str, id, perm: &str| ControlAclEntry {
            scope: "access".into(),
            tag: tag.into(),
            id,
            perm: perm.into(),
        };
        let entries = vec![
            entry("user_obj", None, "rwx"),
            entry("group_obj", None, "r--"),
            entry("group", Some(4444), "-w-"),
            entry("mask", None, "rwx"),
            entry("other", None, "---"),
        ];
        // The caller is a member of both groups, but neither individual
        // entry grants read+write. OR-ing group entries would incorrectly
        // authorize O_RDWR.
        assert_eq!(
            control_acl_allows_access(&entries, 1000, 2000, 1234, &[2000, 4444], 6),
            Some(false)
        );
        assert_eq!(
            control_acl_allows_access(&entries, 1000, 2000, 1234, &[2000, 4444], 4),
            Some(true)
        );
        assert_eq!(
            control_acl_allows_access(&entries, 1000, 2000, 1234, &[2000, 4444], 2),
            Some(true)
        );
    }
}
