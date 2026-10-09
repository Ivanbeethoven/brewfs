//! Explicit object principal wiring; Secret labels are not server policy proofs.
use anyhow::{anyhow, bail, Context as _};
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};

use super::crd::WorkspaceClusterSpec;
use crate::crd::BrewFSCluster;

const ACCESS_KEY: &str = "accessKey";
const SECRET_KEY: &str = "secretKey";

// Intentionally no Debug/Serialize implementation: callers must not log keys.
pub struct AdminObjectCredentials {
    pub access_key: String,
    pub secret_key: String,
}

pub async fn load_admin_object_credentials(
    client: &Client,
    namespace: &str,
    cluster: &BrewFSCluster,
    spec: &WorkspaceClusterSpec,
) -> anyhow::Result<AdminObjectCredentials> {
    let (runtime_name, admin_name) = spec.object_secret_names().map_err(anyhow::Error::msg)?;
    let secrets = Api::<Secret>::namespaced(client.clone(), namespace);
    let runtime = secrets
        .get(runtime_name)
        .await
        .context("load explicit runtime object Secret")?;
    let admin = secrets
        .get(admin_name)
        .await
        .context("load explicit admin object Secret")?;
    validate_principal_pair(&runtime, &admin, &cluster.spec.rustfs.access_key)
}

fn secret_value(secret: &Secret, key: &str, max_bytes: usize) -> anyhow::Result<String> {
    let bytes = secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .ok_or_else(|| anyhow!("object credential Secret is missing a required key"))?;
    if bytes.0.is_empty() || bytes.0.len() > max_bytes {
        bail!("object credential Secret value has invalid size");
    }
    let value = std::str::from_utf8(&bytes.0).context("object credential value is not UTF-8")?;
    if value.trim() != value || value.chars().any(char::is_control) {
        bail!("object credential value has invalid whitespace/control characters");
    }
    Ok(value.to_owned())
}

fn validate_principal_pair(
    runtime: &Secret,
    admin: &Secret,
    server_access_key: &str,
) -> anyhow::Result<AdminObjectCredentials> {
    let runtime_access = secret_value(runtime, ACCESS_KEY, 256)?;
    let _runtime_secret = secret_value(runtime, SECRET_KEY, 4096)?;
    let access_key = secret_value(admin, ACCESS_KEY, 256)?;
    let secret_key = secret_value(admin, SECRET_KEY, 4096)?;
    if runtime_access == access_key
        || runtime_access == server_access_key
        || access_key == server_access_key
    {
        bail!("runtime, admin, and object server root principals must be distinct");
    }
    // These are configuration checks only. Real server-side runtime DELETE
    // denial and positive admin controls remain mandatory acceptance evidence.
    Ok(AdminObjectCredentials {
        access_key,
        secret_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::ByteString;
    use std::collections::BTreeMap;

    fn secret(access: &[u8], value: &[u8]) -> Secret {
        Secret {
            data: Some(BTreeMap::from([
                (ACCESS_KEY.into(), ByteString(access.to_vec())),
                (SECRET_KEY.into(), ByteString(value.to_vec())),
            ])),
            ..Default::default()
        }
    }

    #[test]
    fn shared_or_server_root_object_principals_are_rejected() {
        let runtime = secret(b"runtime", b"runtime-value");
        let admin = secret(b"admin", b"admin-value");
        assert!(validate_principal_pair(&runtime, &admin, "root").is_ok());
        assert!(validate_principal_pair(&runtime, &runtime, "root").is_err());
        assert!(validate_principal_pair(&runtime, &admin, "runtime").is_err());
        assert!(validate_principal_pair(&runtime, &admin, "admin").is_err());
    }

    #[test]
    fn incomplete_empty_oversized_and_non_utf8_credentials_fail_closed() {
        let admin = secret(b"admin", b"admin-value");
        assert!(validate_principal_pair(&Secret::default(), &admin, "root").is_err());
        for value in [
            vec![],
            vec![0xff],
            vec![b'x'; 4097],
            b" secret".to_vec(),
            b"bad\nvalue".to_vec(),
        ] {
            assert!(validate_principal_pair(&secret(b"runtime", &value), &admin, "root").is_err());
        }
    }
}
