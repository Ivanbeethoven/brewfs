use std::collections::BTreeMap;

use axum::http::StatusCode;
use dav_server::fs::{DavProp, FsError};
use serde::{Deserialize, Serialize};

pub const XATTR_DEAD_PROPS: &str = "brewfs.dav.deadprops";
const MAX_DEAD_PROPS_SIZE: usize = 64 * 1024;
const MAX_PROP_XML_SIZE: usize = 32 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredProp {
    name: String,
    prefix: Option<String>,
    namespace: Option<String>,
    xml: Option<Vec<u8>>,
}

impl From<DavProp> for StoredProp {
    fn from(prop: DavProp) -> Self {
        Self {
            name: prop.name,
            prefix: prop.prefix,
            namespace: prop.namespace,
            xml: prop.xml,
        }
    }
}

impl From<StoredProp> for DavProp {
    fn from(prop: StoredProp) -> Self {
        Self {
            name: prop.name,
            prefix: prop.prefix,
            namespace: prop.namespace,
            xml: prop.xml,
        }
    }
}

fn key(prop: &StoredProp) -> (Option<String>, String) {
    (prop.namespace.clone(), prop.name.clone())
}

fn decode(raw: Option<&[u8]>) -> Result<BTreeMap<(Option<String>, String), StoredProp>, FsError> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    if raw.len() > MAX_DEAD_PROPS_SIZE {
        return Err(FsError::GeneralFailure);
    }
    let props: Vec<StoredProp> = serde_json::from_slice(raw).map_err(|error| {
        tracing::error!(error = %error, "invalid WebDAV dead-property xattr");
        FsError::GeneralFailure
    })?;
    let mut indexed = BTreeMap::new();
    for prop in props {
        indexed.insert(key(&prop), prop);
    }
    Ok(indexed)
}

pub fn list(raw: Option<&[u8]>, do_content: bool) -> Result<Vec<DavProp>, FsError> {
    Ok(decode(raw)?
        .into_values()
        .map(|mut prop| {
            if !do_content {
                prop.xml = None;
            }
            prop.into()
        })
        .collect())
}

pub fn get(raw: Option<&[u8]>, requested: &DavProp) -> Result<Vec<u8>, FsError> {
    let key = (requested.namespace.clone(), requested.name.clone());
    decode(raw)?
        .remove(&key)
        .and_then(|prop| prop.xml)
        .ok_or(FsError::NotFound)
}

type PatchStatus = (StatusCode, DavProp);
type ApplyResult = (Option<Vec<u8>>, Vec<PatchStatus>);

pub fn apply(raw: Option<&[u8]>, patch: Vec<(bool, DavProp)>) -> Result<ApplyResult, FsError> {
    let mut props = decode(raw)?;
    let requested = patch.clone();
    let mut statuses = Vec::with_capacity(patch.len());
    for (set, prop) in patch {
        let stored = StoredProp::from(prop.clone());
        if set {
            if stored
                .xml
                .as_ref()
                .is_some_and(|xml| xml.len() > MAX_PROP_XML_SIZE)
            {
                return Ok((
                    None,
                    requested
                        .into_iter()
                        .map(|(_, prop)| (StatusCode::INSUFFICIENT_STORAGE, prop))
                        .collect(),
                ));
            }
            props.insert(key(&stored), stored);
            statuses.push((StatusCode::OK, prop));
        } else {
            let status = if props.remove(&key(&stored)).is_some() {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            };
            statuses.push((status, prop));
        }
    }

    let encoded = serde_json::to_vec(&props.into_values().collect::<Vec<_>>())
        .map_err(|_| FsError::GeneralFailure)?;
    if encoded.len() > MAX_DEAD_PROPS_SIZE {
        return Ok((
            None,
            requested
                .into_iter()
                .map(|(_, prop)| (StatusCode::INSUFFICIENT_STORAGE, prop))
                .collect(),
        ));
    }
    Ok((Some(encoded), statuses))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(name: &str, xml: Option<&[u8]>) -> DavProp {
        DavProp {
            name: name.to_string(),
            prefix: Some("x".to_string()),
            namespace: Some("urn:test".to_string()),
            xml: xml.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn patch_round_trip_and_remove() {
        let value = b"<x:color xmlns:x=\"urn:test\">blue</x:color>";
        let (encoded, statuses) =
            apply(None, vec![(true, prop("color", Some(value)))]).expect("apply property");
        assert_eq!(statuses[0].0, StatusCode::OK);
        let encoded = encoded.expect("encoded properties");
        assert_eq!(get(Some(&encoded), &prop("color", None)).unwrap(), value);

        let (empty, statuses) =
            apply(Some(&encoded), vec![(false, prop("color", None))]).expect("remove property");
        assert_eq!(statuses[0].0, StatusCode::OK);
        assert!(list(empty.as_deref(), true).unwrap().is_empty());
    }

    #[test]
    fn removing_missing_property_reports_not_found() {
        let (_, statuses) =
            apply(None, vec![(false, prop("missing", None))]).expect("bounded result");
        assert_eq!(statuses[0].0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn oversized_property_is_rejected_transactionally() {
        let huge = vec![b'x'; MAX_PROP_XML_SIZE + 1];
        let (encoded, statuses) =
            apply(None, vec![(true, prop("huge", Some(&huge)))]).expect("bounded result");
        assert!(encoded.is_none());
        assert_eq!(statuses[0].0, StatusCode::INSUFFICIENT_STORAGE);
    }
}
