use super::api::CsiResourceQuery;
use async_trait::async_trait;
use serde::Serialize;
use std::{fmt, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CsiSummary {
    pub storageclasses: usize,
    pub persistentvolumes: usize,
    pub persistentvolumeclaims: usize,
    pub pods: usize,
    pub unhealthy_mounts: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CsiResourceList {
    pub items: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsiAdapterError {
    Disabled,
    Unsupported(&'static str),
}

#[async_trait]
pub trait CsiAdapter: fmt::Debug + Send + Sync {
    async fn summary(&self) -> Result<CsiSummary, CsiAdapterError>;

    async fn storageclasses(&self) -> Result<CsiResourceList, CsiAdapterError>;

    async fn persistentvolumes(&self) -> Result<CsiResourceList, CsiAdapterError>;

    async fn persistentvolumeclaims(
        &self,
        query: &CsiResourceQuery,
    ) -> Result<CsiResourceList, CsiAdapterError>;

    async fn pods(&self, query: &CsiResourceQuery) -> Result<CsiResourceList, CsiAdapterError>;
}

pub fn default_csi_adapter(config: super::ConsoleCsiConfig) -> Arc<dyn CsiAdapter> {
    Arc::new(UnsupportedCsiAdapter { config })
}

#[derive(Debug)]
struct UnsupportedCsiAdapter {
    config: super::ConsoleCsiConfig,
}

impl UnsupportedCsiAdapter {
    fn unavailable_or_unsupported<T>(&self, message: &'static str) -> Result<T, CsiAdapterError> {
        if self.config.enabled {
            Err(CsiAdapterError::Unsupported(message))
        } else {
            Err(CsiAdapterError::Disabled)
        }
    }
}

#[async_trait]
impl CsiAdapter for UnsupportedCsiAdapter {
    async fn summary(&self) -> Result<CsiSummary, CsiAdapterError> {
        self.unavailable_or_unsupported("CSI dashboard adapter is not implemented yet")
    }

    async fn storageclasses(&self) -> Result<CsiResourceList, CsiAdapterError> {
        self.unavailable_or_unsupported("CSI StorageClass adapter is not implemented yet")
    }

    async fn persistentvolumes(&self) -> Result<CsiResourceList, CsiAdapterError> {
        self.unavailable_or_unsupported("CSI PersistentVolume adapter is not implemented yet")
    }

    async fn persistentvolumeclaims(
        &self,
        _query: &CsiResourceQuery,
    ) -> Result<CsiResourceList, CsiAdapterError> {
        self.unavailable_or_unsupported("CSI PersistentVolumeClaim adapter is not implemented yet")
    }

    async fn pods(&self, _query: &CsiResourceQuery) -> Result<CsiResourceList, CsiAdapterError> {
        self.unavailable_or_unsupported("CSI Pod adapter is not implemented yet")
    }
}
