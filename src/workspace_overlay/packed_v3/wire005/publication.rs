//! Bounded physical validation primitives. Publication authority additionally
//! requires namespace/edge/frame relations and durable lifecycle protection.

mod index_context;
mod inventory;
#[cfg(target_os = "linux")]
pub(crate) mod native_effective;
mod payload;
mod physical;
#[cfg(test)]
mod physical_tests;
#[cfg(test)]
mod real_s3_tests;
mod semantic_facts;
pub use index_context::{
    V3IndexAuditCounts, V3IndexAuditLimits, V3IndexContextAudit, audit_v3_index_contexts,
};
pub(crate) use index_context::{V3StagedObjectVerifier, audit_v3_staged_index_contexts};
pub use payload::{
    AuthenticatedPayload as V3AuthenticatedPayload, PayloadLimits as V3PayloadLimits,
    PayloadSummary as V3PayloadSummary, authenticate_payload as authenticate_v3_payload,
};
pub use physical::{
    V3PhysicalAuditCounts, V3PhysicalAuditLimits, V3PhysicalDependencyAudit,
    audit_v3_physical_dependencies,
};
