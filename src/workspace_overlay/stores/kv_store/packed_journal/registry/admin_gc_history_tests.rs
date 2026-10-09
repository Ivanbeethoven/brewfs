//! The public admin route must preserve the private retirement driver's proof.
use super::*;
use crate::workspace_overlay::stores::kv_store::packed_journal::registry::admin_gc::PackedHistoryVersionRetirementOptions;

impl Fixture {
    pub(super) async fn run_routed(&self) -> Result<PackedHistoryRetirementReport, WorkspaceError> {
        let binding = self.root.binding.as_ref().unwrap();
        self.store
            .retire_packed_binding_history_version(
                self.client.clone(),
                self.budget.clone(),
                routed_options(binding),
            )
            .await
    }
}

fn routed_options(binding: &PackedLowerBindingRecord) -> PackedHistoryVersionRetirementOptions {
    PackedHistoryVersionRetirementOptions {
        workspace_id: binding.workspace_id,
        binding_version: binding.binding.binding_version,
        grace_ns: 100,
        max_native_holds: 32,
        max_current_bindings: 32,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn packed_gc_header_requires_bounded_sidecar_and_never_reads_topology_fallback() {
    for condition in 0..5 {
        let fixture = Fixture::new().await;
        let mut header = VolumeHeader {
            volume_format: "workspace-v1".into(),
            schema_version: WORKSPACE_SCHEMA_VERSION,
            volume_id: Uuid::new_v4(),
            created_at_ns: 100,
        };
        if condition == 2 {
            header.volume_format = "foreign-volume".into();
        }
        if condition == 3 {
            header.schema_version += 1;
        }
        let mut rows = fixture.backend.memory.rows.lock().await;
        // Presence of a large ordinary topology document must not provide an
        // alternate admission path when the required header is absent/invalid.
        rows.insert(CONTROL_KEY.to_vec(), vec![0; 1 << 20]);
        if condition != 1 {
            rows.insert(
                VOLUME_HEADER_KEY.to_vec(),
                if condition == 4 {
                    vec![0; 513]
                } else {
                    encode(&header).unwrap()
                },
            );
        }
        let before = rows.clone();
        drop(rows);
        let checked = fixture
            .store
            .validate_packed_gc_volume_header(&fixture.budget)
            .await;
        assert_eq!(checked.is_ok(), condition == 0);
        assert_eq!(fixture.backend.unbounded_reads.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.backend.bounded_reads.load(Ordering::SeqCst), 1);
        assert_eq!(*fixture.backend.memory.rows.lock().await, before);
        assert_eq!(
            fixture.budget.state().used[V3BudgetPool::Metadata as usize],
            0
        );
    }
}

#[tokio::test]
async fn packed_gc_history_route_observes_grace_deletes_and_resumes_tombstone() {
    let fixture = Fixture::new().await;
    let first = fixture.run_routed().await.unwrap();
    assert!(first.observing && !first.retired);
    assert_eq!(first.not_before_ns, 1100);
    fixture.backend.now.store(1099, Ordering::SeqCst);
    assert!(fixture.run_routed().await.unwrap().observing);
    assert_eq!(fixture.root().await.members, 1);
    assert!(fixture.objects.path().join(&fixture.reference.key).exists());
    fixture.backend.now.store(1100, Ordering::SeqCst);
    let retired = fixture.run_routed().await.unwrap();
    assert!(retired.retired && !retired.observing);
    assert_eq!((retired.released_members, retired.deleted_objects), (1, 1));
    assert!(!fixture.objects.path().join(&fixture.reference.key).exists());
    let before = fixture.backend.memory.rows.lock().await.clone();
    let resumed = fixture.run_routed().await.unwrap();
    assert!(resumed.retired);
    assert_eq!((resumed.released_members, resumed.deleted_objects), (0, 0));
    assert_eq!(*fixture.backend.memory.rows.lock().await, before);
}

#[tokio::test]
async fn packed_gc_history_route_rejects_false_route_and_does_not_authorize_root() {
    for mutation in 0..7 {
        let fixture = Fixture::new().await;
        let binding = fixture.root.binding.as_ref().unwrap();
        let mut options = routed_options(binding);
        let key = registry_history_root_key(binding);
        let mut rows = fixture.backend.memory.rows.lock().await;
        let mut false_root = fixture.root.clone();
        match mutation {
            0 => options.workspace_id = WorkspaceId::new(),
            1 => options.binding_version += 1,
            2 => {
                false_root.binding.as_mut().unwrap().workspace_id = WorkspaceId::new();
                rows.insert(key, false_root.encode().unwrap());
            }
            3 => {
                false_root.binding.as_mut().unwrap().binding.binding_version += 1;
                rows.insert(key, false_root.encode().unwrap());
            }
            4 => {
                false_root.incarnation = Uuid::new_v4();
                rows.insert(key, false_root.encode().unwrap());
            }
            5 => {
                false_root.revision += 1;
                rows.insert(key, false_root.encode().unwrap());
            }
            _ => {
                rows.remove(&packed_history_key(
                    binding.workspace_id,
                    options.binding_version,
                ));
            }
        }
        let before = rows.clone();
        drop(rows);
        assert!(
            fixture
                .store
                .retire_packed_binding_history_version(
                    fixture.client.clone(),
                    fixture.budget.clone(),
                    options,
                )
                .await
                .is_err(),
            "false route case {mutation} acquired authority"
        );
        assert_eq!(*fixture.backend.memory.rows.lock().await, before);
        assert_eq!(fixture.backend.write_attempts.load(Ordering::SeqCst), 0);
        assert!(fixture.objects.path().join(&fixture.reference.key).exists());
    }
}

#[tokio::test]
async fn packed_gc_history_route_rejects_foreign_closed_exhausted_and_cancelled_budget() {
    for condition in 0..4 {
        let fixture = Fixture::new().await;
        fixture
            .store
            .configure_packed_reader_pin_budget(fixture.budget.clone())
            .unwrap();
        let binding = fixture.root.binding.as_ref().unwrap();
        let options = routed_options(binding);
        let budget = if condition == 0 {
            V3MountBudget::defaults()
        } else {
            fixture.budget.clone()
        };
        let blocked = if condition == 2 {
            let limits = crate::workspace_overlay::packed_v3::wire005::V3BudgetLimits::default();
            Some(
                fixture
                    .budget
                    .admit(&[(
                        V3BudgetPool::Metadata,
                        limits.bytes[V3BudgetPool::Metadata as usize],
                    )])
                    .unwrap(),
            )
        } else {
            None
        };
        if condition == 1 {
            budget.close();
        }
        if condition == 3 {
            options.cancel.cancel();
        }
        let before = fixture.backend.memory.rows.lock().await.clone();
        let reads = fixture.backend.bounded_reads.load(Ordering::SeqCst);
        assert!(
            fixture
                .store
                .retire_packed_binding_history_version(fixture.client.clone(), budget, options,)
                .await
                .is_err()
        );
        assert_eq!(fixture.backend.bounded_reads.load(Ordering::SeqCst), reads);
        assert_eq!(*fixture.backend.memory.rows.lock().await, before);
        assert!(fixture.objects.path().join(&fixture.reference.key).exists());
        drop(blocked);
        assert_eq!(
            fixture.budget.state().used[V3BudgetPool::Metadata as usize],
            0
        );
    }
}

#[tokio::test]
async fn packed_gc_history_route_resume_cannot_bypass_current_or_orphan_claim() {
    for target_current in [true, false] {
        let fixture = Fixture::new().await;
        fixture.make_committed_fixture().await;
        let binding = fixture.root.binding.as_ref().unwrap();
        let mut rows = fixture.backend.memory.rows.lock().await;
        rows.insert(
            packed_claim_key(binding.workspace_id),
            PACKED_CLAIM.to_vec(),
        );
        if target_current {
            rows.insert(
                packed_current_key(binding.workspace_id),
                binding.encode().unwrap(),
            );
        }
        let before = rows.clone();
        drop(rows);
        assert!(matches!(
            fixture.run_routed().await,
            Err(WorkspaceError::Fenced)
        ));
        assert_eq!(*fixture.backend.memory.rows.lock().await, before);
        assert_eq!(fixture.root().await.members, 1);
        assert!(fixture.objects.path().join(&fixture.reference.key).exists());
    }
}
