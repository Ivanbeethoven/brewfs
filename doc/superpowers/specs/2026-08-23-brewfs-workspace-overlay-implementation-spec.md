# BrewFS Workspace Overlay 实现规范

2026-10-04当前任务范围仅v3：本规范的隔离、POSIX、fencing、publication/recovery与GC
继续用于三创新系统验收；旧flat/workspace格式兼容不再是出口。具体边界见
[当前v3目标](../plans/2026-10-04-brewfs-all-spec-completion.md)。

- 状态：Implementation spec / 与当前代码同步
- 日期：2026-08-23
- 功能名：Workspace Overlay
- Cargo feature：`workspace-overlay`
- volume format：`workspace-v1`
- catalog 格式：`catalog_format: 2`（entity-key 布局）
- 首要生产 backend：Redis、TiKV
- 本地语义与故障注入 backend：SQLite / SQLx

### 2026-10-19 交付模型：不可变 lower + 不轮转的可写 overlay

代码基线：`codex/packed-metadata-aliyun-20260930` 的 `0d4e36c`（PR #41）。
PR #39 已合入 per-layer extent generation 与 data group commit；PR #41 只增加
新建 inode 的完整路径 memo，没有实现 create group commit。交付状态、已知失败及复现命令见
[交接文档](../plans/2026-10-17-overlay-no-seal-handoff.md)。

当前实现只有一种模型：每个 mount 是 **一个不可变 lower + 一个可写 overlay**。

- 不存在 seal 事务、flatten/compaction 物化和 strict lower 模式；对应代码、store 接口
  （`begin_seal`/`advance_seal`/`hash_seal`/`commit_seal`/`abort_recoverable_seal`/
  `fast_forward_commit`/`install_compaction`）与 CLI `workspace snapshot`、
  `workspace commit`、`fork <workspace-uuid>` 已删除。
- 新的不可变 lower 只来自 packed publication（pack 上传 -> 认证 manifest -> binding
  install/publish）。`SealJournal`/`SealPhase` 类型名保留，仅表示 packed publication journal。
- 挂载中的 head 永不轮转：任何改变 `head_layer_id`/`head_epoch` 的路径都必须持有该
  workspace 唯一的 active writable lease（§15.3）。
- lower 在 attach 时认证一次；之后 lower inode、权限输入、反向别名与 `(parent, name)`
  查找按 binding 生命周期 memo，不再逐操作重新认证。
- lease/唯一写者安全保持不变（§6.5、§15）。

下文凡与本节冲突处以本节为准；已删除的历史设计不再保留在规范中。

### workspace-v1 与 packed-v3 的边界

本文控制面契约适用于现有 `workspace-v1` 不可变 lower/delta 实现；“与代码同步”不表示
后续所有packed格式或新worktree都已全量审计。它不是004/005对象的wire定义，也不能由
standalone packed readonly mount推导本规范的fork/publication/recovery/GC已支持packed lower。

[packed-v3 SPEC](2026-09-27-brewfs-packed-metadata-v3-readonly-smallfiles.md)负责当前packed-v3
对象与只读plan；当前 packed lower 的接入、publication/recovery 与性能交付状态以
[维护中的架构文档](../../architecture/workspace-overlay.md)及交接文档为准，不再将整个
packed lower 集成标为未开始。既有 `BaseRevision` 的layer/version/
root_hash和bincode bytes保持不变；不能把root_hash暗解释成PM07 object digest。新binding
需独立版本化、capability检查和原子head/binding切换；旧workspace不查询它，错误不回退。

下面§9.6的“未覆盖范围为零”仅在完整native fixed layer pair已全部resolve后成立。
组合packed fallback时upper未覆盖是 **Absent**，显式Hole/whiteout/truncate mask才阻断
lower；fallback完成前不能调用补hole的兼容resolver将Absent丢失。同epoch内写入还需要
inode data_version/sequence或等价locking，head_epoch/lease guard本身不是read一致性保证。

## 1. 实现目标

在 BrewFS 中增加可 fork 的 workspace view：多个 workspace 共享不可变 lower 和
immutable slice/block，每个 workspace 只保存自己的 namespace、inode 属性和数据 extent
变更。

v1 必须支持：

1. 创建 workspace volume（空 lower 或 packed-v3 lower）；
2. 从精确不可变 revision 创建/fork workspace，挂载和销毁 workspace；
3. workspace 内完整的核心 POSIX mutation 隔离；
4. path 级 diff；
5. discard；
6. lease-aware GC；
7. crash recovery 与 stale writer fencing；
8. 通过 packed publication 产生新的不可变 lower（不封存挂载中的 workspace）。

## 2. 不可破坏的兼容契约

以下均为硬约束：

1. `workspace-overlay` 不加入默认 Cargo features；
2. 默认构建不编译 workspace module；
3. 现有 flat metadata schema 不增加字段或迁移；
4. `SliceDesc` 的字段、serde/rkyv 编码和 golden bytes 不改变；
5. object key 继续使用 `chunks/{slice_id}/{block_index}`；
6. flat cache key、dirty writeback key 和现有后台任务不增加 workspace 维度；
7. 普通 volume 不查询 workspace schema，不启动 workspace lease/watch/GC；
8. legacy volume 不允许通过 mount 参数直接转换成 workspace volume；
9. flat-only binary 遇到 `workspace-v1` volume 必须返回
   `FeatureNotCompiled("workspace-overlay")`；
10. workspace backend 的错误不得回退为 flat semantics。

## 3. Feature 与启动分流

### 3.1 Cargo feature

在根 `Cargo.toml` 增加：

```toml
[features]
default = ["fuse-io-uring-runtime"]
workspace-overlay = ["dep:bincode", "dep:blake3"]
```

workspace-only dependency 必须声明为 `optional = true` 并由该 feature 开启。v1 应优先复用
现有依赖，不新增必选 dependency。

### 3.2 Module 声明

`src/lib.rs` 与 `src/main.rs`：

```rust
#[cfg(feature = "workspace-overlay")]
pub mod workspace_overlay;
```

### 3.3 Volume marker

配置增加 `volume_format`，旧配置缺省为 `flat-v1`。composition root 先依据配置分流；普通
配置不探测或查询 workspace schema。

workspace path 随后从独立 header 读取不可变 marker 并交叉校验：

```text
volume_format = "workspace-v1"
schema_version = 1
volume_id = UUID
```

不得通过猜测 delta 表是否存在来判断 format。配置声明 `workspace-v1` 但 header 缺失或
不匹配时返回 corruption；配置声明 flat 时不读取该 header。

### 3.4 Mount-time 分流

只允许在 composition root 分流一次：

```rust
match volume_format {
    VolumeFormat::FlatV1 => mount_flat(config).await,
    #[cfg(feature = "workspace-overlay")]
    VolumeFormat::WorkspaceV1 => mount_workspace(config).await,
    #[cfg(not(feature = "workspace-overlay"))]
    VolumeFormat::WorkspaceV1 => Err(FeatureNotCompiled("workspace-overlay")),
}
```

flat path 保持：

```text
MetaStore -> MetaClient<MetaStore> -> VFS<S, MetaClient<R>>
```

workspace path 使用：

```text
WorkspaceStore -> WorkspaceMetaLayer<W> -> VFS<S, WorkspaceMetaLayer<W>>
```

选择完成后，不在每个 FUSE syscall 中检查 `workspace_enabled`。

## 4. 目录与依赖边界

新增：

```text
src/workspace_overlay/
  mod.rs
  error.rs
  ids.rs
  model.rs
  catalog.rs
  digest.rs
  resolver/
    mod.rs
    chain.rs
    dentry.rs
    inode.rs
    extent.rs
    xattr.rs
  meta_layer/
    mod.rs
    namespace.rs
    attributes.rs
    data.rs
    locks.rs
  lifecycle/
    mod.rs                  # fork/discard、mount session、heartbeat
    lease_validity.rs
    packed_mount_session.rs
  publish/
    mod.rs
    binding.rs              # packed lower binding install/publish
    diff.rs
  stores/
    mod.rs
    database.rs
    kv_backend.rs
    kv_store.rs
    redis.rs
    tikv.rs
  cache.rs
  gc.rs
  control.rs
  test_base_fixture.rs      # 仅测试：播种非空 flat lower
  metrics.rs

src/chunk/read_plan.rs
```

依赖规则：

```text
workspace_overlay -> meta::MetaLayer / chunk / vfs stable types
workspace_overlay -> WorkspaceStore implementations
chunk::read_plan  -X-> workspace_overlay
flat meta/vfs     -X-> workspace_overlay
composition root  -> flat path + workspace path
```

`src/chunk/read_plan.rs` 是 feature-gated 的中立执行类型，不得包含 `WorkspaceId`、`LayerId`
或 workspace backend 逻辑。

## 5. 核心类型

所有 ID 使用 newtype，禁止在内部 API 中传裸 `String`：

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LayerId(Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct LeaseId(Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct JournalId(Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(Uuid);
```

### 5.1 BaseRevision

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BaseRevision {
    pub layer_id: LayerId,
    pub sealed_version: u64,
    pub root_hash: [u8; 32],
}
```

### 5.2 ViewContext

一个 writable mount 绑定一个 workspace：

```rust
pub struct ViewContext {
    pub workspace_id: WorkspaceId,
    pub head_layer_id: LayerId,
    pub head_epoch: u64,
    pub lease_id: LeaseId,
    pub holder_generation: u64,
}
```

`head_epoch` 只在持有该 workspace 唯一 active writable lease 的 packed publication
（binding install/publish、native publication complete）或 clean restore 中增加。挂载中的
head 不会被其他 holder 改变（§15.3）。普通 metadata mutation 不增加 head epoch。

### 5.3 状态枚举

```rust
pub enum WorkspaceState {
    Active,
    Quiescing,
    Sealing,
    Deleting,
    Error,
}

pub enum LayerState {
    Writable,
    Sealing,
    Sealed,
    Deleting,
}

pub enum LeaseState {
    Active,
    Releasing,
    Released,
    Expired,
}
```

`Sealed` layer（即不可变 lower）永不可修改；active workspace 恰好有一个 writable head 和
一个不可变 lower。固定不变量为 `writable(depth=2) -> sealed(depth=1) -> null`。不存在把
非平坦链压平的路径：mount 遇到非固定层对直接返回 `CorruptMetadata`。

`WorkspaceState::{Quiescing, Sealing}`、`LayerState::Sealing` 的 discriminant 仍保留以
解码旧记录，不再由任何新代码路径写入。

### 5.4 持久 enum 编码

持久 discriminant 固定如下，后续不得重排：

```text
WorkspaceState: Active=0, Quiescing=1, Sealing=2, Deleting=3, Error=4
LayerState:     Writable=0, Sealing=1, Sealed=2, Deleting=3
LeaseState:     Active=0, Releasing=1, Released=2, Expired=3
DentryOp:       Put=0, Whiteout=1
InodeState:     Present=0, Deleted=1
ValueOp:        Put=0, Whiteout=1
ExtentKind:     Data=0, Hole=1
SealPhase:      Prepare=0, Quiesced=1, DataDrained=2, Hashed=3,
                HeadSwitched=4, Completed=5, Aborted=6
```

`SealPhase`/`SealJournal` 现只承载 packed publication journal 的状态；名称因持久编码
保留。

未知值返回 `UnsupportedSchemaVersion` 或 `CorruptMetadata`，不得映射到默认状态。

## 6. WorkspaceStore 接口

`WorkspaceStore` 与现有 `MetaStore` 完全独立：

```rust
#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> WorkspaceStoreCapabilities;

    async fn initialize_workspace_schema(&self) -> Result<(), WorkspaceError>;
    async fn load_workspace(&self, id: WorkspaceId) -> Result<WorkspaceRecord, WorkspaceError>;
    async fn load_layer(&self, id: LayerId) -> Result<LayerRecord, WorkspaceError>;
    async fn load_layer_chain(&self, head: LayerId) -> Result<Vec<LayerRecord>, WorkspaceError>;

    async fn create_volume_root(&self, request: CreateVolumeRoot) -> Result<WorkspaceRecord, WorkspaceError>;
    async fn create_workspace(&self, request: CreateWorkspace) -> Result<WorkspaceRecord, WorkspaceError>;
    async fn create_snapshot(&self, request: CreateSnapshot) -> Result<SnapshotRecord, WorkspaceError>;
    async fn load_snapshot(&self, id: SnapshotId) -> Result<SnapshotRecord, WorkspaceError>;
    async fn delete_snapshot(&self, id: SnapshotId) -> Result<(), WorkspaceError>;

    async fn acquire_lease(&self, request: AcquireLease) -> Result<SnapshotLease, WorkspaceError>;
    async fn renew_lease(&self, request: RenewLease) -> Result<SnapshotLease, WorkspaceError>;
    async fn release_lease(&self, request: ReleaseLease) -> Result<(), WorkspaceError>;

    async fn get_dentry_deltas(&self, request: DentryQuery) -> Result<Vec<DentryDelta>, WorkspaceError>;
    async fn get_inode_deltas(&self, request: InodeQuery) -> Result<Vec<InodeDelta>, WorkspaceError>;
    async fn get_extent_deltas(&self, request: ExtentQuery) -> Result<Vec<DataExtentDelta>, WorkspaceError>;
    async fn get_xattr_deltas(&self, request: XattrQuery) -> Result<Vec<XattrDelta>, WorkspaceError>;
    async fn get_acl_deltas(&self, request: AclQuery) -> Result<Vec<AclDelta>, WorkspaceError>;

    async fn apply_namespace_mutation(&self, request: NamespaceMutation) -> Result<MutationResult, WorkspaceError>;
    async fn apply_inode_mutation(&self, request: InodeMutation) -> Result<FileAttr, WorkspaceError>;
    async fn append_data_extent(&self, request: AppendDataExtent) -> Result<(), WorkspaceError>;
    async fn apply_xattr_mutation(&self, request: XattrMutation) -> Result<(), WorkspaceError>;
    async fn apply_acl_mutation(&self, request: AclMutation) -> Result<(), WorkspaceError>;
    async fn apply_lock_mutation(&self, request: LockMutation) -> Result<(), WorkspaceError>;

    async fn install_packed_lower_binding(&self, request: InstallPackedLowerBinding) -> Result<PackedLowerBindingRecord, WorkspaceError>;
    async fn publish_packed_lower_binding(&self, request: PublishPackedLowerBinding) -> Result<PackedLowerBindingRecord, WorkspaceError>;
    async fn load_seal_journal(&self, id: JournalId) -> Result<SealJournal, WorkspaceError>; // packed publication journal
    async fn list_seal_journals(&self, workspace: WorkspaceId) -> Result<Vec<SealJournal>, WorkspaceError>;

    async fn mark_workspace_deleting(&self, request: MarkDeleting) -> Result<(), WorkspaceError>;

    async fn list_gc_roots(&self) -> Result<GcRoots, WorkspaceError>;
    async fn list_layers(&self, cursor: GcCursor) -> Result<LayerPage, WorkspaceError>;
    async fn delete_layer_metadata(&self, request: DeleteLayerMetadata) -> Result<(), WorkspaceError>;
}
```

### 6.1 Mutation guard

所有 mutation 请求必须携带：

```rust
pub struct HeadGuard {
    pub workspace_id: WorkspaceId,
    pub expected_head_layer_id: LayerId,
    pub expected_head_epoch: u64,
    pub lease_id: LeaseId,
    pub holder_generation: u64,
}
```

backend 在同一事务中检查：

1. workspace state 为 `Active`；
2. head ID 和 epoch 匹配；
3. head state 为 `Writable`；
4. lease 为 `Active` 且未过期；
5. holder generation 匹配。

任一不匹配返回 `WorkspaceError::Fenced`，不得重试到新 head。packed mount 的 mutation CAS
另外检查 `workspace.active_lease`、packed writer-authority/open 记录，并以
`lease.expires_at_ns` 作为 CAS 截止时间：lease 过期后旧 holder 的任何提交都失败。
head ID/epoch 在挂载期间不变（§15.3），其相等检查只是唯一写者保证的冗余防线。

### 6.2 Capability

```rust
pub struct WorkspaceStoreCapabilities {
    pub atomic_head_switch: bool,
    pub durable_lease: bool,
    pub transactional_namespace_mutation: bool,
    pub transactional_rename: bool,
    pub watch_head_change: bool,
}
```

v1 mount 要求前四项均为 true；缺失时返回 `UnsupportedCapability`。

### 6.3 Redis/TiKV 事务 substrate

Redis 与 TiKV 共用 `WorkspaceKvBackend` 和 `KvWorkspaceStore<B>`，不得复制两套
`WorkspaceStore` 状态机。backend 必须提供：

```rust
async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, WorkspaceError>;
async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, WorkspaceError>;
async fn compare_and_swap(
    &self,
    checks: &[KvCheck],
    writes: &[KvWrite],
) -> Result<bool, WorkspaceError>;
async fn server_time_ns(&self) -> Result<i64, WorkspaceError>;
```

`compare_and_swap` 必须原子检查全部 expected value 并应用全部 put/delete；条件不匹配返回
`false`，不得产生部分写。

持久记录使用 bincode，并带固定 envelope magic `BWSKV001`。未知 magic、解码失败或
schema version 不匹配必须 fail-closed。

逻辑 key schema（entity-key 布局，entity 键是实体的唯一副本，不存在镜像层）：

```text
control                                                   { schema_version, header, catalog_format }
ws/<workspace-id>                                         WorkspaceRecord
layer/<layer-id>                                          LayerRecord
lease/<workspace-id>/<lease-id>                           SnapshotLease
journal/<workspace-id>/<journal-id>                       SealJournal
snapshot/<snapshot-id>                                    SnapshotRecord
alloc/<allocator-name>                                    分配器计数
delta/dentry/<layer>/<parent-ino-order-key>/<hex-name>
delta/inode/<layer>/<ino-order-key>
delta/xattr/<layer>/<ino-order-key>/<hex-name>
delta/acl/<layer>/<ino-order-key>/<acl-type>/<acl-id-order-key>
delta/extent/<layer>/<ino-order-key>/<chunk-index>/<sequence>
```

`control` 只在创建卷与目录迁移时写入，不随任何拓扑操作更新。lease 与 journal 键按
workspace 归组，支持精确的前缀列举。

普通 mutation 在同一事务检查 workspace record、head layer record 与该 workspace 的
lease record，并提交 delta key；native `renew_lease` 只 CAS 对应 lease record；allocator
只 CAS 对应 `alloc/<name>` record。不同 workspace 的热路径不得读取、锁定或写入其他
workspace 的键，也不得共享进程内 mutex。
packed mount 的 joint heartbeat 在一个 CAS 中续期该 workspace 的 lease 与 open authority。

packed mutation 还比较 `control`，用于 fence 无需 lease 的 schema migration，但不写它。
`CommitViewCache` 可复用上次成功提交的 view 与 derived bytes，省去 view pre-read。
cached CAS 保留 workspace/head/lease/writer-authority/open/CONTROL 六个 view checks，
另保留本次 reverse-index、extent generation 和 slice-deletion checks；被省略的 lower、
PWB3 current/claim/history 与 recovery rows 由唯一 live lease 及不轮转 head 的不变量保护。
slice deletion 的 absent 猜测也必须成为真实 CAS 条件。lease deadline 仍在 backend time
检查；cached conflict/Fenced 或 derived Busy 时重新读取并完整校验，有界重试。
首次提交与 recovery-token writer 始终读取新 view；backend uncertainty 不盲目重放。

inode、chunk 和 sequence 使用固定宽度、保持数值顺序的十六进制 component；文件名和
xattr name 使用原始 bytes 的 hex，禁止 UTF-8 有损转换。layer 删除必须先进入
`Deleting`，再按 6.6 节的两阶段协议删除 delta 键与 layer record。

Redis 物理前缀为 `{brewfs-ws-v1}:<workspace-namespace>:ws:v1/`。固定 hash tag 保证
Redis Cluster 中 Lua 涉及的所有 key 位于同一 slot；CAS 使用 binary-safe Lua，lease
到期时间使用 Redis `TIME`。

TiKV 物理前缀为 `<workspace-namespace>/ws:v1/`。CAS 使用 pessimistic transaction 和
`get_for_update` 锁定本次 CAS 的实体 keys，冲突执行有界重试并带指数退避；prefix scan
使用同一只读事务和分页 range。lease 的当前时间必须来自 `TransactionClient::current_timestamp()` 的
PD TSO physical 毫秒分量并安全换算为纳秒，禁止使用 mount host wall clock。

`workspace-namespace` 只允许 ASCII 字母、数字、`-`、`_`、`.`，为空或包含其他字符时
启动失败。不同 volume 必须使用不同 namespace。

### 6.4 拓扑事务（read-tracked）

fork、snapshot pin、discard、GC、租约获取与释放、packed publication 通过读追踪事务执行：

```rust
let mut txn = store.topology_txn();
let workspace = txn.read_workspace(workspace_id).await?;
let layer = txn.read_layer(head_layer_id).await?;
// 校验并修改 txn 中的实体，然后一次性提交
txn.commit().await?;
```

规则：

- 每次 `read_*` 使用 `get_many` 精确读取并把原始值计入读取集；提交时检查集等于读取集，
  写入集由实际修改的实体自动生成；
- `put`/`delete` 只允许作用于已声明读取的键，未声明的写入直接返回
  `CorruptMetadata`，从接口上消除忘记声明的依赖；
- 提交调用 `compare_and_swap(checks, writes)`，一次 CAS 原子提交全部检查与写入，外部只能
  观察到提交前或提交后状态；
- 冲突重试时在最新值上重新读取并回放操作；
- 任何操作不得触碰其他 workspace 的键；不同 workspace 的 fork、publication、GC 可以并行，无关
  workspace 的心跳与热写不进入本事务的检查集；
- 不存在进程内全局拓扑 gate；不同 workspace 的拓扑并发由 backend 多键 CAS 串行化，互不
  阻塞；
- store 层冲突重试使用指数退避加 jitter（基数 1 ms、上限 100 ms）；`hot_mutation`、
  `renew_lease`、`allocate_id` 使用同一退避策略。

各操作的检查集与写入集：

| 操作 | 检查集 | 写入集 |
|---|---|---|
| `acquire_lease` / `release_lease` | ws record、本 lease | ws record（`active_lease`）、lease record |
| packed binding install/publish | ws record、head layer、本 workspace 的 active lease、base layer、PWB3 current/history、root generation | ws（head epoch）、head layer、PWB3 current/history |
| `create_workspace` / `create_snapshot` | base layer（须 `Sealed`） | ws record 与 head layer / snapshot |
| GC 删除 | 目标 layer（CAS 至 `Deleting`） | layer record、delta 键 |

### 6.5 单活跃 writable lease

每个 workspace 最多一个 active writable lease，由 `WorkspaceRecord.active_lease:
Option<LeaseId>` 承载互斥：获取租约时 CAS 该字段 `None -> Some(lease_id)`，释放与过期
回收时清除。`acquire_lease` 必须识别指向已失效租约的悬挂指针并自动清理，防止任何终止
路径漏清导致 workspace 无法挂载。热路径的 fencing 直接校验 lease record 本身。

### 6.6 GC 删除协议与终止态清理

GC 不依赖全局一致快照，使用两阶段删除加提交点复核：

1. 扫描根集合（workspace、snapshot、lease、非终态 publication journal）与 layer，得出可达集合与待删除候选
   （允许陈旧）；
2. 对每个候选 layer 以读取到的精确值 CAS 标记 `Deleting`；
3. 重新扫描根集合复核：在标记前提交的根创建（fork、snapshot、mount、journal）在复核中
   可见，发现引用则放弃删除并把 layer 恢复为 `Sealed`；在标记之后尝试的根创建，其对
   base layer 精确值的 CAS 检查必然失败；
4. 复核通过后删除 delta 键与 layer record。

根创建操作（fork、snapshot、`acquire_lease`、packed publication）在其事务中 CAS 检查所引用
base layer 的精确值（含 `state == Sealed`），这是上述协议的另一半。

GC 周期删除超过宽限期的 `Released`/`Expired` lease 与 `Completed`/`Aborted` journal
（每个 workspace 保留最近一条 journal 供 `inspect`），目录规模与活跃实体数量挂钩。

### 6.7 目录迁移

`initialize_workspace_schema` 负责目录格式迁移，由 CLI 显式触发：

```text
brewfs workspace --meta-backend redis --meta-url <url> migrate
brewfs workspace --meta-backend sqlx --meta-url <url> migrate
brewfs workspace --meta-backend tikv --meta-tikv-pd-endpoints <pd-endpoints> migrate
```

TiKV 用 `--meta-tikv-pd-endpoints` 指定 PD endpoints；`--meta-url` 只供 Redis 与
SQLite backend 使用。

迁移规则：

- 读取 `control` 检测目录格式；旧格式文档包含实体表时，把各实体写入对应 entity 键，并
  CAS 切换为仅含卷头与 `catalog_format: 2` 的新文档；
- 迁移由 `control` 单键 CAS 保证只执行一次，中途崩溃可续跑；
- 迁移完成前 mount 被拒绝，错误提示指向上述 `migrate` 命令；
- `migrate` 与 mount 必须选择同一个 backend、endpoint 与 `workspace-namespace`；
- 迁移为单向：迁移完成后旧版本二进制无法读取新格式目录，发布说明必须说明所有 reader
  升级完成后才能执行 `migrate`。

## 7. 数据库 schema

本节定义 SQLite 参考实现。SQLite 表统一使用 `ws_v1_` 前缀，不得修改现有 flat 表；
Redis/TiKV 使用 6.3 节的 backend-neutral key schema。

### 7.1 Volume header

```sql
CREATE TABLE ws_v1_volume_header (
    singleton_id          INTEGER PRIMARY KEY CHECK(singleton_id = 1),
    volume_format         TEXT NOT NULL,
    schema_version        INTEGER NOT NULL,
    volume_id             BLOB NOT NULL,
    created_at_ns         INTEGER NOT NULL
);
```

该表只由 workspace path 读取。初始化时 header 最后插入；已存在 header 时字段不可修改。

### 7.2 Workspace

```sql
CREATE TABLE ws_v1_workspaces (
    workspace_id          BLOB PRIMARY KEY,
    active_lease          BLOB,
    head_layer_id         BLOB NOT NULL,
    head_epoch            INTEGER NOT NULL,
    fork_base_layer_id    BLOB,
    fork_base_version     INTEGER,
    fork_base_root_hash   BLOB,
    owner_id              TEXT,
    state                 INTEGER NOT NULL,
    created_at_ns         INTEGER NOT NULL,
    updated_at_ns         INTEGER NOT NULL
);
```

### 7.3 Layer

```sql
CREATE TABLE ws_v1_layers (
    layer_id              BLOB PRIMARY KEY,
    parent_layer_id       BLOB,
    state                 INTEGER NOT NULL,
    schema_version        INTEGER NOT NULL,
    sealed_version        INTEGER,
    delta_digest          BLOB,
    root_hash             BLOB,
    depth                 INTEGER NOT NULL,
    owner_workspace_id    BLOB,
    next_sequence         INTEGER NOT NULL,
    owned_slice_count     INTEGER NOT NULL DEFAULT 0,
    owned_bytes           INTEGER NOT NULL DEFAULT 0,
    created_at_ns         INTEGER NOT NULL,
    sealed_at_ns          INTEGER
);

CREATE INDEX ws_v1_layers_parent_idx ON ws_v1_layers(parent_layer_id);
```

`parent_layer_id` 创建后不可修改。只有 writable layer 的 `owner_workspace_id` 可以执行
mutation。

### 7.4 Dentry delta

```sql
CREATE TABLE ws_v1_dentry_delta (
    layer_id              BLOB NOT NULL,
    parent_ino            INTEGER NOT NULL,
    name                  BLOB NOT NULL,
    op                    INTEGER NOT NULL,
    ino                   INTEGER,
    entry_type            INTEGER,
    sequence              INTEGER NOT NULL,
    PRIMARY KEY(layer_id, parent_ino, name)
);

CREATE INDEX ws_v1_dentry_parent_idx
    ON ws_v1_dentry_delta(layer_id, parent_ino);
```

`op` 为 `Put` 或 `Whiteout`。name 按原始字节保存；进入 FUSE/MetaLayer 边界时继续遵守当前
BrewFS 的名称校验规则。

### 7.5 Inode delta

```sql
CREATE TABLE ws_v1_inode_delta (
    layer_id              BLOB NOT NULL,
    ino                   INTEGER NOT NULL,
    state                 INTEGER NOT NULL,
    kind                  INTEGER NOT NULL,
    size                  INTEGER NOT NULL,
    mode                  INTEGER NOT NULL,
    uid                   INTEGER NOT NULL,
    gid                   INTEGER NOT NULL,
    rdev                  INTEGER NOT NULL,
    nlink                 INTEGER NOT NULL,
    atime_ns              INTEGER NOT NULL,
    mtime_ns              INTEGER NOT NULL,
    ctime_ns              INTEGER NOT NULL,
    symlink_target        BLOB,
    parent_hint           INTEGER,
    data_version          INTEGER NOT NULL,
    sequence              INTEGER NOT NULL,
    PRIMARY KEY(layer_id, ino)
);
```

第一次修改 lower inode 时只 copy-up有效 `FileAttr`，不复制文件数据。

### 7.6 Xattr 与 ACL delta

```sql
CREATE TABLE ws_v1_xattr_delta (
    layer_id              BLOB NOT NULL,
    ino                   INTEGER NOT NULL,
    name                  BLOB NOT NULL,
    op                    INTEGER NOT NULL,
    value                 BLOB,
    sequence              INTEGER NOT NULL,
    PRIMARY KEY(layer_id, ino, name)
);

CREATE TABLE ws_v1_acl_delta (
    layer_id              BLOB NOT NULL,
    ino                   INTEGER NOT NULL,
    acl_type              INTEGER NOT NULL,
    acl_id                INTEGER NOT NULL,
    op                    INTEGER NOT NULL,
    value                 BLOB,
    sequence              INTEGER NOT NULL,
    PRIMARY KEY(layer_id, ino, acl_type, acl_id)
);
```

remove 操作写 `Whiteout`，不能删除 upper row。

### 7.7 Data extent delta

```sql
CREATE TABLE ws_v1_data_extent_delta (
    layer_id              BLOB NOT NULL,
    ino                   INTEGER NOT NULL,
    chunk_index           INTEGER NOT NULL,
    logical_offset        INTEGER NOT NULL,
    length                INTEGER NOT NULL,
    kind                  INTEGER NOT NULL,
    slice_id              INTEGER,
    slice_offset          INTEGER,
    sequence              INTEGER NOT NULL,
    PRIMARY KEY(layer_id, ino, chunk_index, sequence)
);

CREATE INDEX ws_v1_extent_lookup_idx
    ON ws_v1_data_extent_delta(layer_id, ino, chunk_index, sequence);
```

约束：

- `Data`：`slice_id` 与 `slice_offset` 非空；
- `Hole`：两者为空；
- offset/length 均为 chunk-local；
- `length > 0`；
- `logical_offset + length <= chunk_size`；
- 同一 layer 内 sequence 单调递增。

### 7.8 Lease

```sql
CREATE TABLE ws_v1_snapshot_leases (
    lease_id              BLOB PRIMARY KEY,
    workspace_id          BLOB NOT NULL,
    base_layer_id         BLOB NOT NULL,
    base_version          INTEGER NOT NULL,
    base_root_hash        BLOB NOT NULL,
    holder_generation     INTEGER NOT NULL,
    writable              INTEGER NOT NULL,
    state                 INTEGER NOT NULL,
    expires_at_ns         INTEGER NOT NULL,
    created_at_ns         INTEGER NOT NULL,
    updated_at_ns         INTEGER NOT NULL
);

CREATE UNIQUE INDEX ws_v1_one_writer_idx
    ON ws_v1_snapshot_leases(workspace_id)
    WHERE writable = 1 AND state = 0;
```

v1 每个 workspace 最多一个 writable lease。`writable` 字段为后续 read-only mount 预留；
v1 mount 请求只能创建 `writable=1`。snapshot 通过 `ws_v1_snapshots` 本身成为 GC root，不能
直接挂载；需要读取 snapshot 时先 fork 成 workspace。

### 7.9 Snapshot

```sql
CREATE TABLE ws_v1_snapshots (
    snapshot_id           BLOB PRIMARY KEY,
    name                  TEXT,
    layer_id              BLOB NOT NULL,
    sealed_version        INTEGER NOT NULL,
    root_hash             BLOB NOT NULL,
    owner_id              TEXT,
    created_at_ns         INTEGER NOT NULL
);

CREATE UNIQUE INDEX ws_v1_snapshot_name_idx
    ON ws_v1_snapshots(name)
    WHERE name IS NOT NULL;
```

snapshot 是 sealed `BaseRevision` 的持久 GC root。删除 snapshot 只删除 root reference，由 GC
决定 layer/object 回收。

### 7.10 Journal 与 allocator

journal 表/键只由 packed publication 使用（native freeze、recovery claim、complete
driver），不存在 workspace seal journal。

```sql
CREATE TABLE ws_v1_seal_journal (
    journal_id            BLOB PRIMARY KEY,
    workspace_id          BLOB NOT NULL,
    old_head_layer_id     BLOB NOT NULL,
    expected_head_epoch   INTEGER NOT NULL,
    phase                 INTEGER NOT NULL,
    pending_bytes         INTEGER NOT NULL,
    delta_digest          BLOB,
    root_hash             BLOB,
    new_head_layer_id     BLOB,
    last_error            TEXT,
    created_at_ns         INTEGER NOT NULL,
    updated_at_ns         INTEGER NOT NULL
);

CREATE TABLE ws_v1_allocators (
    name                  TEXT PRIMARY KEY,
    next_value            INTEGER NOT NULL
);
```

allocator 至少包含 `inode`、`sealed_version`。slice ID 继续使用现有 volume-global allocator。

## 8. Layer digest

创建 volume root、packed publication 冻结 native source 时计算：

```text
delta_digest = BLAKE3(canonical layer delta stream)
root_hash    = BLAKE3(schema_version || parent_root_hash || delta_digest)
```

canonical stream 规则：

1. 表顺序固定为 dentry、inode、xattr、acl、extent；
2. 每张表按完整 primary key 升序；
3. integer 使用 big-endian 固定宽度；
4. bytes/string 使用 `u64 length + raw bytes`；
5. nullable 字段先写一字节 presence；
6. enum 写稳定的显式 `u8` discriminant；
7. schema version 进入 digest；
8. 不使用 JSON、Debug 输出或数据库行的隐式顺序。

`digest.rs` 提供唯一的编码实现和 golden fixtures。

## 9. Resolver

resolver 必须是无 I/O 的纯逻辑；store 负责批量加载固定 layer pair 和 delta，resolver 只处理
已加载 records。

### 9.1 Fixed layer pair

`load_layer_chain(head)` 为兼容 store contract 保留，但挂载只能接受：

```text
[writable head, immutable lower]
```

校验：

- 长度必须为 2；
- head 为 writable、depth=2、parent 指向 base；
- base 为 `Sealed`（不可变 lower）、depth=1、parent 为空；不平坦即 `CorruptMetadata`，不存在压平路径；
- point lookup 使用一次 Redis `MGET` 或 TiKV batch-get 读取两个完整 key；
- named dentry/inode/xattr/ACL 查询不得调用 prefix scan。

### 9.2 Lookup

对 `(parent_ino, name)` 从 newest 向 root：

1. 命中 `Put`：返回 ino/type；
2. 命中 `Whiteout`：返回 `None`；
3. 未命中：继续 lower；
4. base 未命中：返回 `None`。

### 9.3 Readdir

1. 对 upper 和 base 查询 `(layer_id, parent_ino)` 的局部索引；
2. newest-first 插入 `BTreeMap<name, Winner>`；
3. 已有 winner 的 name 不再被 lower 覆盖；
4. whiteout 进入 winner map，但最终不输出；
5. 输出按当前 BrewFS readdir 稳定排序规则；
6. `.` 与 `..` 继续由 VFS/MetaLayer 当前语义生成，不存 dentry delta。

### 9.4 Inode

从 newest 向 root 返回第一个 inode row：

- `Present` 返回属性；
- `Deleted` 返回不存在；
- 未命中继续 lower。

写属性前先 resolve effective inode，再将完整结果 copy-up 到 head 后修改目标字段。

### 9.5 Xattr/ACL

按 `(ino, name)` 或 `(ino, type, id)` newest-wins。Whiteout 停止 lower lookup。

### 9.6 Extent

workspace 持久类型：

```rust
pub enum ExtentKind {
    Data { slice_id: u64, slice_offset: u64 },
    Hole,
}

pub struct LayerExtent {
    pub layer_id: LayerId,
    pub logical_offset: u64,
    pub length: u64,
    pub sequence: u64,
    pub kind: ExtentKind,
}
```

resolve 指定 chunk range：

1. `uncovered = requested range`；
2. layer newest-first；
3. 同一 layer sequence 从大到小；
4. extent 与 uncovered 相交的部分成为 winner；
5. `Data` winner 计算：

```text
resolved.slice_offset = extent.slice_offset + intersection.start - extent.logical_offset
```

6. `Hole` winner 只从 uncovered 中切除，对应结果保持为零；
7. 未被任何 extent 覆盖的范围为零；
8. 最终 plan 按 logical offset 升序、无重叠。

必须使用 checked arithmetic；overflow 返回 `CorruptMetadata`。

## 10. Chunk read plan 接缝

`src/chunk/read_plan.rs`：

```rust
pub enum ReadPlanSegment {
    Data {
        logical_offset: u64,
        length: u64,
        slice_id: u64,
        slice_offset: u64,
    },
    Zero {
        logical_offset: u64,
        length: u64,
    },
}

pub struct ResolvedReadPlan {
    pub segments: Vec<ReadPlanSegment>,
}
```

executor：

1. 调用方 buffer 先清零；
2. `Zero` 不触发 BlockStore 请求；
3. `Data` 使用 `block_span_iter_slice(slice_offset, length, layout)`；
4. block key 仍为 `(slice_id, block_index)`；
5. 继续使用现有 clean cache、singleflight、checksum 和并发读取；
6. 每个 segment 写入互不重叠的 buffer range；
7. plan 越界或重叠返回 `InvalidReadPlan`。

### 10.1 Backend provider

feature 内新增中立 trait：

```rust
#[async_trait]
pub trait WorkspaceReadPlanProvider: Send + Sync {
    async fn read_plan(
        &self,
        ino: i64,
        chunk_index: u64,
        offset: u64,
        len: u64,
    ) -> Result<ResolvedReadPlan, MetaError>;

    async fn range_has_data(
        &self,
        ino: i64,
        offset: u64,
        len: u64,
    ) -> Result<bool, MetaError>;
}
```

`WorkspaceMetaLayer<W>` 实现该 trait。

`Backend` 在 feature build 下增加：

```rust
workspace_read_plan: Option<Arc<dyn WorkspaceReadPlanProvider>>
```

- 当前 `Backend::new` 设置为 `None`；
- 新增 `Backend::new_workspace` 设置 provider；
- feature 未启用时该字段和相关代码不编译。

### 10.2 FileReader

`read_chunk_span_into`：

```text
provider == None:
    保持当前 chunk_slices -> DataFetcher::read_at_into_from_slices

provider != None:
    provider.read_plan(...)
    chunk::read_plan::execute_into(...)
```

workspace path 不写入当前 `chunk_slices: DashMap<u64, Vec<SliceDesc>>`。有效 plan cache 由
`WorkspaceMetaLayer` 管理，key 为：

```text
(workspace_id, head_epoch, ino, chunk_index, inode_data_version)
```

write/truncate/punch-hole 后增加 inode `data_version` 并删除对应 plan cache。

VFS 当前 `range_has_committed_slices()` 在 workspace provider 存在时必须改调
`range_has_data()`；flat path 保持当前 `get_slices()` 实现。

### 10.3 Prepared unified 接口与 packed 集成边界

中立provider现有可选 `supports_prepared_unified_read` / `prepare_unified_read` 返回
`PreparedUnifiedRead {UnifiedReadPlan, Arc<dyn UnifiedReadSourceFetcher>}`；VFS使用
`execute_unified_into`，成功交付后计logical bytes。packed-v3 readonly 与 packed-lower
workspace 已走该接缝；native workspace 仍可以使用§10.1–10.2的旧slice计划。
接口存在本身不代替 publication/recovery/GC 与实际 mount 的验收证据。

packed lower需保持absolute/chunk-relative offsets明确，preserve raw names、cold attrs、
hardlinks和holes，upper fully covered时无需lower GET。packed publication 不得把未覆盖lower bytes
省略成holes；普通fsync/close只做workspace durability，显式packed publish才repack。
读取fence、packed graph可达性GC、journal/lease/head-CAS和实际双mount必须独立验收。

## 11. WorkspaceMetaLayer

`WorkspaceMetaLayer<W: WorkspaceStore>` 实现现有 `MetaLayer`。它持有：

```rust
pub struct WorkspaceMetaLayer<W> {
    store: Arc<W>,
    view: ArcSwap<ViewContext>,
    resolver_cache: WorkspaceResolverCache,
    root_ino: AtomicI64,
    session: WorkspaceSession,
}
```

### 11.1 禁止的实现方式

- 不在 `WorkspaceMetaLayer` 内调用 flat `MetaStore` mutation；
- 不让未实现方法静默委托给 flat store；
- 不通过 downcast 判断具体 backend；
- 不把 workspace ID编码进 inode、chunk ID 或 slice ID；
- 不缓存不带 `head_epoch/data_version` 的有效 view。

### 11.2 MetaLayer 方法覆盖清单

以下必须在 v1 实现，不得返回 `NotImplemented`：

```text
initialize/stat_fs
stat/stat_fresh/stat_for_open
lookup/lookup_with_attr/lookup_path/readdir/opendir
mkdir/rmdir/create_file/create_node/link/symlink/unlink
rename/rename_exchange/rename_with_flags/can_rename
set_file_size/extend_file_size/truncate/fallocate_file
get_names/get_dentries/get_dir_parent/get_paths/read_symlink
set_attr/chmod/chown/open/close
write/append_slice/invalidate_chunk_slices
next_id
start_session/shutdown_session
get_plock/set_plock/get_flock/set_flock
set_xattr/get_xattr/list_xattr/remove_xattr
set_acl/get_acl
```

workspace `get_slices` 无法无损表达 Hole 与裁剪后的 `slice_offset`，因此明确返回
`NotSupported("workspace view requires read_plan")`。所有已知调用点必须在 workspace path
改用 `WorkspaceReadPlanProvider`：FileReader 使用 `read_plan()`，
`range_has_committed_slices()` 使用 `range_has_data()`，flat compactor 在 workspace mount
禁用。不得返回丢失 Hole 的伪 slice list。

其他 maintenance 方法：

- `get_deleted_files()` 返回空集合，删除回收由 workspace GC 管理；
- `remove_file_metadata()` 返回 NotSupported，只允许 GC 通过 WorkspaceStore 删除；
- `stat_fs()` 不得为每次调用遍历 layer graph。Redis、TiKV、SQLite 尚未持久化并在 mutation
  transaction 内原子更新 usage counter 时，必须明确返回 `NotSupported`，不得伪报零 usage
  或返回非一致快照；计数器落地后再返回 volume-wide capacity/inode limit，并分别通过
  workspace metrics 报告 private/shared bytes。

### 11.3 Cache

独立 cache：

```text
effective inode:  (head_epoch, ino, inode_version)
dentry:           (head_epoch, parent_ino, name)
readdir:           (head_epoch, parent_ino, dir_version)
read plan:         (head_epoch, ino, chunk_index, data_version)
xattr/ACL:         (head_epoch, ino, attribute_version)
```

packed lower 在 attach 时认证一次，inode、权限输入、反向别名与 lookup memo 按 binding
生命周期保留。可变数据 cache 的权威来自 `LeaseValidity`，不是 TTL：

- upper inode/dentry/xattr 与 permission snapshot 在每次 commit 前后失效受影响的 keys；
- namespace cache 保存完整 paths 与已验证 aliases，data-only commit 不清空它；create
  只失效新 inode 的 paths/aliases，其他 namespace mutation 清空 namespace cache；
- invalidation generation 阻止并发读取在 commit 后重新插入旧 rows；view change 清空
  可变 caches；失去 lease proof 后清空并绕过 upper/namespace/permission caches；
- `LeaseLocalFence` 同时验证 lease 仍有效与 inode cached rows 未变，不能由 lower memo
  代替；cached permission snapshot 可以用于构建 mutation，但仍必须 CAS expected head/lease；
- 新 inode 的唯一 dentry 已在 create 中提交，可从 parent 的完整 cached paths 加 name
  seed 其 paths；root path 已知，其他 parent 未命中时不 seed。没有 seed inode/xattr rows。

默认 `BREWFS_WORKSPACE_ROW_CACHE_TTL_MS=60000`，在 live lease proof 下只限制保留时间。
lower memo 不因 lease proof 丢失而变成写权限；所有读 fence 与写 CAS 仍然生效。

## 12. POSIX mutation

所有多 row 操作在一个 WorkspaceStore transaction 中执行，并使用同一个 `HeadGuard`。

### 12.1 Create/mkdir/symlink/mknod

1. resolve parent 并校验权限；
2. resolve 同名 dentry，存在则返回 `AlreadyExists`；
3. 从 volume-global inode allocator 取新 inode；
4. head 写 `InodeDelta::Present`；
5. head 写 `DentryDelta::Put`；
6. 更新 parent mtime/ctime/version；
7. 同一事务提交。

### 12.2 Unlink

1. resolve dentry 与 inode；
2. head 写 dentry Whiteout；
3. copy-up inode 并减少 nlink；
4. nlink 到零时写 `InodeDelta::Deleted`，但 open handle 仍保留运行期引用；
5. 更新 parent；
6. close 最后一个 handle 后才允许 GC inode 私有数据。

### 12.3 Rmdir

除 unlink 步骤外，必须对 effective readdir 判空。只检查 head rows 不足以判断 lower child。

### 12.4 Rename

一个事务内：

1. resolve source、destination 和两个 parent；
2. 校验类型、权限、sticky bit、目录循环和 replace 规则；
3. source 写 Whiteout；
4. destination 写 Put；
5. 被替换 destination 更新 nlink/deleted state；
6. 目录移动更新 parent hint 与 `..` 语义；
7. 更新两个 parent 时间/version；
8. commit。

`RENAME_EXCHANGE` 同时写两个 Put，不产生短暂缺失状态。

### 12.5 Hardlink

inode number 不变；head 增加 dentry Put，并 copy-up inode 增加 nlink。child workspace 的
nlink 变化不得影响 sibling。

### 12.6 Setattr/xattr/ACL

resolve effective value，copy-up 到 head 后修改。remove 写 Whiteout。

### 12.7 Locks

flock/plock key 必须包含 `workspace_id`。不同 workspace 的同 inode 不互相阻塞；同一
workspace 的多个 handle 继续遵守当前 BrewFS lock 语义。

### 12.8 Atime

沿用当前 BrewFS mount 的 atime/relatime/noatime 策略，但任何实际发生的 atime mutation 都
必须 copy-up 到 workspace head。不得更新 sealed lower inode。性能测试需要单独记录纯读取
是否因为 atime 产生 private metadata；agent workspace 推荐默认 relatime 或 noatime。

## 13. 数据写入

### 13.1 普通 write

现有 DataWriter 继续创建新 `slice_id` 并上传 immutable block。提交 metadata 时，
`WorkspaceMetaLayer::write` 转换：

```text
SliceDesc {
  slice_id,
  chunk_id,
  offset,
  length
}

=>

DataExtentDelta::Data {
  logical_offset = offset,
  length,
  slice_id,
  slice_offset = 0,
  sequence = next_layer_sequence
}
```

同一 transaction 更新 inode size、mtime/ctime、data_version。不得向 flat slice table
append。

### 13.1.1 Group commit（2026-10-19，PR #39）

校验后的 `MetaLayer::write` 进入每个 mount 的 FIFO 队列；同一时刻只有一个 leader，
先取出至多 64 个 write，再持有 mount-wide mutation gate，将合法成员组成**一个**
`VersionedMutation`：每个不同 inode 一行（按队列顺序取 size 最大值、递增 data_version，
使用 batch 的 mtime/ctime），每个 extent 的 visible prefix 按队列顺序排列。
它仍 CAS expected head，原 row allocator 仍递增 `next_sequence`，extent sequence
保留重叠范围 newest-wins 顺序；head counter、lease 与 GC checks 没有删除。
没有 sequence block、head-free commit 或 fresh-SID GC exemption。

- inode 已不存在或不是普通文件的成员单独失败，不影响同批其他 write；
- batch 失败时每个合法成员收到相同 errno；
- write 只在其 batch 提交后返回，fsync 语义不变；
- create/unlink/rename/link/setattr/open/close 仍各自持有 gate 提交。

测试：`group_committed_writes_land_individually_and_fail_individually`、
`group_commit_same_file_keeps_order_and_size`、
`concurrency_oracle_no_lost_creates_or_size_updates`。

### 13.2 Truncate

shrink `old_size -> new_size`：

1. 更新 inode size；
2. 对 `[new_size, old_size)` 按 chunk 拆分并追加 Hole extent；
3. 增加 data_version；
4. 同一事务提交。

再次 extend 时未写区域读取为零，lower bytes 不得复活。

truncate(0) v1 可以写覆盖已知有效范围的 Hole；后续优化可增加 chunk/file opaque marker，
但不得改变可见语义。

### 13.3 Punch hole / zero range

- `FALLOC_FL_PUNCH_HOLE`：追加 Hole，不改变 size；
- extend beyond EOF：只更新 size，新范围自然为零；
- `ZERO_RANGE`：v1 使用 Hole 语义；若未来实现物理预分配，另行增加 extent kind；
- keep-size/preallocation 必须保持当前 stat/statfs 约定。

### 13.4 Write ordering

同一 writable head 内使用当前 VFS/FUSE write ordering，并由 layer `sequence` 提供持久顺序。
sequence 在追加 metadata 的事务内分配；不得根据 slice ID推断顺序。

### 13.5 Metadata 可见性与 durability

普通 write 提交 extent 前，新 slice 必须满足以下之一：

1. object block 已 remote durable；或
2. 已进入现有可 crash-recover 的 durable writeback staging/journal，且 staging key 可由
   workspace recovery 唯一定位。

packed publication 不接受第二种状态：发布前必须 quiesce VFS 并确认所有引用 slice
remote durable。fork 只接受已存在的不可变 revision，不涉及 dirty 数据。

若 object upload 成功后 HeadGuard 被 fence，metadata 不提交，该 slice 记录为 orphan并由
grace GC 回收。不得把 mutation 自动重放到新 head。

### 13.6 Workspace dirty key

workspace persistent writeback/recovery key 必须包含：

```text
(volume_id, workspace_id, head_epoch, ino, chunk_index, local_sequence, writer_epoch)
```

workspace staging 目录使用：

```text
<cache-root>/workspace-v1/<volume-id>/<workspace-id>/<head-epoch>/...
```

不得修改 flat dirty key 格式。mount 时验证解析后的绝对 cache path 位于配置的 cache root
内部。

## 14. Workspace 生命周期

### 14.1 创建 workspace volume

事务内：

1. 初始化 `ws_v1_*` schema；
2. 分配 root inode；
3. 创建 root sealed layer `L0`；
4. root layer 写 root inode；
5. 计算 root digest/revision；
6. 创建 writable layer `L1(parent=L0)`；
7. 创建默认 workspace 指向 L1；
8. 写 volume marker。

marker 最后写。marker 写入前失败可重试初始化；marker 存在但 schema 不完整视为 corruption。

### 14.2 Mount

1. 读取 format marker；
2. 加载 workspace/head；
3. 校验 chain；
4. 获取 writable 或 read-only lease（自动清理指向已失效租约的悬挂 `active_lease`）；
5. 构造 `ViewContext`；
6. 构造 `WorkspaceMetaLayer`；
7. 构造 `Backend::new_workspace`；
8. 启动 lease heartbeat；
9. mount 成功后对外返回 session token。

任何后续步骤失败必须释放 lease。agent 不获得 metadata/object backend credential。

### 14.3 新的不可变 lower

不存在 seal/restack。新的不可变 lower 只由 packed publication 产生：

1. 由 operator/admin 在持有该 workspace 唯一 writable lease 的前提下 quiesce VFS；
2. 冻结 native source 并上传 packed-v3 对象；
3. 认证 manifest，得到 `VerifiedPackedLower`；
4. 在一次 CAS 中 install/publish binding（检查 lease、head、base、PWB3 current/history）；
5. head epoch 只在该 CAS 中增加。

挂载中的普通 mount 不调用以上任何一步。

### 14.4 Fork / Create

输入必须是精确的不可变 `BaseRevision`。不存在“从 active workspace fork”：
`fork <workspace-uuid>` 已删除，因为它需要先封存 live workspace。

事务内为每个 child：

1. 创建 writable head，parent=base.layer_id；
2. 创建 workspace record；
3. 保存 fork base 的 layer/version/hash；
4. 不遍历 inode、extent、slice 或 object。

child mount 时再获取 lease。

### 14.5 Snapshot

snapshot 只 pin 一个已存在的不可变 revision（来自 packed publication），不封存 workspace：

1. 创建 `SnapshotRecord`（CAS 检查 base layer 精确值）；
2. 同名 snapshot 返回 `AlreadyExists`，不覆盖旧 snapshot。

CLI 不提供 `workspace snapshot`；operator 的 `BrewFSWorkspaceSnapshot` 只对 packed
workspace 生效，native workspace 的 snapshot 请求进入 `Failed`。

### 14.6 Discard

1. 禁止新 mount/mutation；
2. writable lease 必须释放或被管理员显式 fence；
3. workspace 标记 Deleting；
4. 删除 workspace head root reference；
5. 由后台 GC 回收不可达 layer/object；
6. 不同步递归删除 parent。

### 14.7 Commit

不存在 fast-forward commit（`workspace commit` 已删除）：它需要先封存 source workspace。

## 15. Lease 与 fencing

### 15.1 Heartbeat

- 默认 TTL：30 秒；
- heartbeat：10 秒；
- GC grace：至少 2 × TTL；
- heartbeat 只允许相同 holder generation 延长；
- wall clock 由 backend/server 产生，不信任客户端时间；
- native heartbeat 执行 `renew_lease`；packed mount 使用 joint heartbeat，续期 native
  lease 与 packed open authority，两者成功后才延长 `LeaseValidity` 的本地 deadline
  （请求起点 + TTL）。失败永久 revoke proof 并终止 heartbeat，后续写入 fail closed；
- 过期租约回收不在 heartbeat 中进行，集中在 `acquire_lease` 的本 workspace 顺路清理、
  GC 周期的全局回收；不存在 `recover_incomplete_seals` 路径。

### 15.2 Holder generation

每次 mount/session 重建分配新的 generation。recovery 后旧 generation 的 heartbeat、write
全部返回 `Fenced`。

### 15.3 Head 不在 active lease 下改变

所有改变 `head_layer_id`/`head_epoch` 的路径都要求调用方出示该 workspace 唯一的 active
writable lease：

| 路径 | 检查 |
|---|---|
| hot mutation（仅 `next_sequence`） | mutation CAS 中的 `checked_hot_guard` |
| `install_packed_lower_binding` / `publish_packed_lower_binding` | KV `checked_hot_guard`、SQLite `checked_guard` |
| packed journal commit | guard 精确相等 + committed state 复核 |
| initial packed bootstrap | `workspace.active_lease == guard.lease_id` |
| native publication complete driver | CAS 中 `workspace.active_lease == actual.lease` |
| clean restore / headless recovery | 原 lease 必须 `Released`，successor 是唯一 `Active` writable lease |

`acquire_lease` 在已有 live lease 时返回 `Busy`，因此任何 admin 在 mount 持有 workspace
期间都拿不到上述路径所需的权限。旧 epoch mutation 永久失败；调用方不得自动替换 guard
后重放非幂等 mutation。回归测试：`invariant_no_head_change_under_an_active_lease`、
`invariant_second_mount_cannot_acquire_a_live_lease`、
`invariant_commit_after_lease_expiry_fails_closed`、
`invariant_commit_after_lease_loss_fails_closed_and_blocks_takeover`。

## 16. Crash recovery

不存在 workspace seal journal 恢复。重启后：

- 过期 lease 由 `acquire_lease` 与 GC 回收；
- packed publication journal 由 packed recovery（`RecoverPackedMount`、clean restore、
  headless recovery）按其自身 phase 继续或中止；
- object 已上传但 metadata 未引用：进入 orphan grace；
- metadata 不得引用不存在或未 durable 的 object；
- 无法证明状态时将 workspace 设为 Error，不猜测继续。

## 17. Diff

v1 `workspace diff` 比较 fork base（或 `--against` 的不可变 revision）与当前 writable head：

```rust
pub enum PathChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    MetadataOnly,
}

pub struct PathChange {
    pub path: Vec<u8>,
    pub kind: PathChangeKind,
    pub old_path: Option<Vec<u8>>,
    pub ino: Option<i64>,
    pub changed_ranges: Vec<Range<u64>>,
}
```

规则：

- dentry Whiteout -> Deleted；
- Put 且 base 无同名 -> Added；
- inode/extent/xattr/ACL delta -> Modified/MetadataOnly；
- 相同 inode 从一个 path whiteout 到另一 path Put -> Renamed；
- 无法唯一识别 rename 时输出 delete + add；
- changed_ranges 来自 Data/Hole extents 的逻辑范围合并；
- diff 只读 metadata，不下载 object payload。

## 18. GC

### 18.1 Roots

mark roots：

- 所有 workspace head；
- 所有 active/releasing lease 的 base；
- 所有非终态 packed publication journal 引用；
- 所有管理员 snapshot/tag。

### 18.2 Layer mark

从 root 沿 parent 标记 reachable layer。不可达 layer 超过 grace period 后按 6.6 节的
两阶段协议删除：先以精确值 CAS 标记 `Deleting`，重新扫描根集合复核，通过后删除 delta 键
与 layer record；复核发现新引用则恢复为 `Sealed`。GC 同时删除超过宽限期的
`Released`/`Expired` lease 与 `Completed`/`Aborted` journal（每个 workspace 保留最近一条
journal 供 `inspect`）。extent commit 复核 slice deletion reservation，挂载内 GC 并发删除
orphan slice 时新 extent 不会引用正在删除的 slice。

### 18.3 Slice mark

不能仅按 `owner_layer` 删除 object，因为多个 layer 可能引用同一 slice。每次 GC
cycle 从所有 reachable layer 的 Data extent 标记 reachable slice ID：

1. mark reachable layers；
2. 扫描其 Data extent，构建 reachable slice set；
3. 删除不可达 layer metadata；
4. 对不在 reachable slice set、没有 dirty/orphan lease 且超过 grace 的 slice 执行现有
   object deletion；
5. 删除失败保留 tombstone 并重试。

不得为 fork 对每个 slice 增加 eager refcount。

### 18.4 Slice 删除 fence（per-layer extent generation，PR #39）

GC 删除 object slice 前必须写入 reservation `packed/v3/native-slice-delete/<sid>`。
reservation 的 CAS 同时检查：

1. layer inventory generation 与全部 layer 记录（census）；
2. census 中**每个** layer 的 extent generation `packed/v3/native-extent-generation/<layer>`；
3. 全量 `delta/extent/` 扫描：任何非 target layer 引用该 slice 则拒绝，即便该 layer
   当前看似不可达；
4. 该 slice key 不存在（或与已有 reservation 完全相同）。

extent 新增、覆盖、删除均在同一 CAS 中递增所写 layer 的 generation；新写入 Data extent
还检查所引用 slice 的 reservation key 不存在。CAS 同时保留 root/topology authority 与
target `Deleting` rows 的精确检查。

安全论证：若 extent 在 reservation 的扫描越过其位置之后、reservation CAS 之前提交，
它已递增所在 layer 的 generation，reservation CAS 失败；若在此期间新建 layer，
inventory generation 已变化，同样失败；若 reservation 先提交，extent commit 的 slice
absent 检查失败。因此不能同时接受未被扫描发现的新引用与 slice 删除。回归测试
`native_slice_extent_landing_behind_the_scan_cursor_blocks_the_reservation`
（去掉 per-layer 检查后该测试失败）。旧的全卷 key 不再写入，但 reservation 仍检查它，
以 fence 旧版本二进制的递增。

per-layer generation 消除了该 counter 上跨 workspace 的 extent commit 竞争；同一 workspace 内仍共享
head layer 的 generation 与 `next_sequence`，由 group commit 摊薄。
该 native collector 仍有小 catalog 限制：最多 120 target layers、4,096 extent rows、
32 MiB scan data；越界返回 `Busy`，不会使用不完整 census 放行。

## 19. Lower materialization

不存在 seal/flatten 物化。新的 lower 只由 packed publication 生成（§14.3）。测试需要非空
flat lower 时使用 `test_base_fixture::install_test_base`（`#[cfg(test)]`），它只能用于离线
workspace，不是生产 API。

## 20. 后台任务

workspace mount/daemon 只启动 feature module 内的：

```text
lease heartbeat（只执行 renew_lease，不回收租约）
layer GC（含终止态 lease/journal 清理）
orphan object GC
```

不得为 workspace mount 启动当前基于 raw flat MetaStore 的 CompactionWorker/GC。普通 mount
仍只启动现有任务。

## 21. Control API 与 CLI

feature 启用时增加：

```text
brewfs workspace --meta-backend <sqlx|redis|tikv> \
  [--meta-url <url>] \
  [--meta-tikv-pd-endpoints <pd...>] \
  [--workspace-namespace <name>] <command>

brewfs workspace init-volume
brewfs workspace init-packed-volume --manifest-key <key> --data-config <file>
brewfs workspace migrate
brewfs workspace create [--from <revision>]
brewfs workspace fork <revision> --count <n>
brewfs workspace list
brewfs workspace inspect <workspace>
brewfs workspace diff <workspace> [--against <revision>]
brewfs workspace discard <workspace>

brewfs mount <mountpoint> --workspace <workspace-id>
```

workspace CLI 和 mount 必须选择同一个 backend、endpoint 与 `workspace-namespace`。
workspace 模式明确拒绝 etcd；普通 flat mount 的 backend 选择保持不变。
`migrate` 把旧格式目录迁移到 entity-key 布局（见 6.7 节），迁移完成后旧版本二进制无法读取
该目录，发布说明必须说明所有 reader 升级完成后才能执行 `migrate`。

revision 外部编码：

```text
<layer-uuid>:<sealed-version>:<root-hash-hex>
```

API 不接受只有 layer ID 的 fork precondition。`workspace snapshot`、`workspace commit` 与
`fork <workspace-uuid>` 已删除：它们都需要封存 live workspace。

### 21.1 Inspect 输出

至少包含：

```text
workspace_id/state/owner
head_layer_id/head_epoch
base_revision/layer_depth
lease_id/holder_generation/expires_at
private metadata rows/private bytes/shared bytes
pending writeback
last packed publication journal phase/error
```

## 22. Error 类型

```rust
pub enum WorkspaceError {
    FeatureNotCompiled(&'static str),
    UnsupportedVolumeFormat(String),
    UnsupportedSchemaVersion(u32),
    UnsupportedCapability(&'static str),
    WorkspaceNotFound(WorkspaceId),
    LayerNotFound(LayerId),
    LeaseNotFound(LeaseId),
    Busy,
    Fenced,
    Conflict(ConflictDetail),
    LayerDepthLimit { depth: u32, hard_limit: u32 },
    CorruptMetadata(String),
    InvalidReadPlan(String),
    InvalidStateTransition { from: String, to: String },
    Backend(String),
    Io(std::io::Error),
}
```

错误映射到 errno 时：

- Fenced -> `ESTALE`；
- Busy -> `EBUSY`；
- not found -> `ENOENT`；
- conflict -> control API conflict，不映射成普通文件 syscall；
- corruption -> `EIO` 并记录高优先级事件。

## 23. Metrics

```text
brewfs_workspace_mount_total{result}
brewfs_workspace_fork_total{result}
brewfs_workspace_fork_control_latency_seconds
brewfs_workspace_fork_drain_latency_seconds
brewfs_workspace_publish_total{result}
brewfs_workspace_publish_changed_paths
brewfs_workspace_layer_depth
brewfs_workspace_resolver_steps
brewfs_workspace_extent_plan_segments
brewfs_workspace_private_bytes_written
brewfs_workspace_parent_bytes_read
brewfs_workspace_shared_cache_hits
brewfs_workspace_active_leases
brewfs_workspace_fenced_writes_total
brewfs_workspace_gc_reachable_layers
brewfs_workspace_gc_orphan_bytes
```

flat `.stats` 现有字段不得改变；workspace metric 仅在 feature build 注册。

## 24. 测试规范

### 24.1 Default-flat 非干扰 gate

必须自动验证：

1. `cargo test` 默认不编译 workspace module；
2. default dependency tree 不增加 workspace-only dependency；
3. legacy schema snapshot 不变；
4. `SliceDesc` serde/rkyv golden bytes 不变；
5. object key golden tests 不变；
6. default binary help 不出现 workspace 命令；
7. flat workload 的 metadata/object request 数不增加；
8. feature-enabled binary 挂载 flat volume 时 WorkspaceStore mock 调用为零；
9. feature-enabled flat mount 不启动 workspace task；
10. flat-only binary 对 workspace marker fail-closed。

### 24.2 Resolver unit/property tests

- dentry Put/Whiteout/recreate；
- inode Present/Deleted/copy-up；
- xattr/ACL Whiteout；
- data -> hole -> data；
- lower extent 中间裁剪的 slice_offset；
- truncate shrink 后 extend 不复活 lower bytes；
- 多层相同 logical range newest-wins；
- 随机操作与内存 byte-array/tree oracle 比较；
- overflow、zero length、越界和 corrupt chain。

### 24.3 POSIX tests

- 两 workspace 修改同一文件，互不可见；
- create/unlink/recreate；
- rmdir 检查 lower child；
- rename replace/exchange/noreplace；
- directory rename 与 `..`；
- hardlink/nlink；
- open-unlink-read/write-close；
- chmod/chown/xattr/ACL；
- truncate/extend/punch-hole/sparse file；
- mmap、fallocate；
- flock/plock 只在同 workspace 互斥；
- symlink 与循环限制；
- xfstests、pjdfstest 双 workspace harness。

### 24.4 Lifecycle/fencing tests

唯一写者不变量（全部为必过回归）：

- 第二个 mount 不能获取 live lease（`Busy`），原 holder 继续提交；
- lease 过期（backend 时钟）后旧 holder 的提交失败；
- lease 丢失后提交失败，且持有 packed open record 时其他 holder 不能接管；
- 任何 admin 路径都不能在他人 live lease 下改变 head（§15.3）；
- 挂载期间 fork/discard 其他 workspace 不改变挂载 workspace 的 head/epoch；
- stale holder generation heartbeat/write；
- fork 不遍历文件/slice；
- packed publication 各 phase crash 后结果为旧 binding 或新 binding 之一。

### 24.5 GC

- 100 child 共享 lower，删除 99 个不回收 lower；
- active lease 阻止回收；
- expired lease grace；
- 共享 slice 被多个 layer 引用时删除 origin layer 不误删 object；
- CAS replacement 失败保留旧 graph；
- orphan upload grace 后回收；
- GC 重启幂等。

### 24.6 性能

2026-10-19 交付的 r31 / PR #41 已完成测量（overlay/native files/s）：

| Workers | stat | full read | write | readback | mixed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 1.00 | 0.71 | 0.86 | 1.06 | 1.02 |
| 16 | — | — | 0.63 | 0.91 | 0.85 |
| 32 | — | — | 0.57 | 0.88 | 0.87 |

1 worker：500 files/operations，2 repetitions；16/32 workers：2,000 files/operations，
rw order-swapped 合并后每个 arm 6 repetitions。比值为每个 arm 的 files/s 算术均值之比，
不混用 runner summary 中的 median。stat/full 为 100 KiB 文件与第二个 scan epoch，
rw 为 16 KiB、fsync、mixed 30% writes；Redis + RustFS、FUSE TTL0/direct I/O、无压缩、
无 payload cache/prefetch，默认 lease-bound metadata cache。92 rows 无错误且 teardown 完整。

最终 sweep 在 16-worker rw 中被中断，32-worker 未启动。16-worker 已落盘的 stat/full
rows 得到 0.72/0.57，但缺少整个 run 的 summary/cleanup proof，故不列为完成验收。
历史完整 stat/full：16w PR #30 为 0.75/0.57；32w `hot26`（PR #29 前）为 0.79/0.53，
均不是 r31 测量。完整 artifact 路径、重算/复现命令、三个 baseline overlay 失败、
`source_file` flake、operator `double_must_use` 与未验证 smoke 见
[交接文档](../plans/2026-10-17-overlay-no-seal-handoff.md)。

剩余限制：并发 write 仍受单独 create gate 与共享 head/generation 影响；stat/full 的
permission/path/FUSE read 成本仍在。`statfs` usage counter 未实现，native GC census 有界。
没有实现 create batching、sequence block、head-free commit、fresh-SID exemption 或
lower full copy-up；create 与首次 fsync 合并因 `O_EXCL` 可见性被拒绝。
这些 small-file 结果不代替下面的广泛性能要求，也不代表完整 feature suite 已通过。

- 百万文件 base fork 的 metadata mutation 数为常数；
- 修改 10 GiB 文件中 4 KiB 不复制整文件；
- 固定两层 lookup/stat/create/unlink 的 Redis `SCAN` 增量必须为 0；
- point lookup 必须保持一次 backend round trip，且耗时不随无关 key 数量增长；
- 100 child 读同 base block 的 shared cache/singleflight；
- randrw、small-file create、rename、git checkout/build；
- 分开报告 active throughput、fsync/close、fork control latency；
- 并发 fork 的拓扑吞吐与延迟基准；拓扑操作延迟不得随无关 workspace 数量与
  无关写入频率增长；
- default-flat 与 feature-enabled-flat 对照，回归超出噪声阈值即阻断。

### 24.7 CI matrix

```text
cargo fmt --check
cargo clippy --all-targets
cargo test

cargo clippy --all-targets --features workspace-overlay
cargo test --features workspace-overlay
cargo test --all-features

workspace smoke: fuse-io-uring-runtime
workspace smoke: fuse-tokio-runtime
workspace Redis: live distributed catalog contract + 双 mount FUSE + pjdfstest
workspace TiKV: live distributed catalog contract + 双 mount FUSE + pjdfstest
```

### 24.8 拓扑事务并发与目录迁移

- 并发心跳不阻塞 fork/publication；
- 64 个并发 fork 全部成功；
- 拓扑操作 × 心跳 / × 本 workspace 写入 / × 其他 workspace 写入的冲突矩阵；
- GC 与根创建竞争两个方向（故障注入）；
- 超过宽限期的 `Released`/`Expired` lease 与 `Completed`/`Aborted` journal 被清理；
- v1 目录迁移后实体完整、中途崩溃可续跑，迁移完成前 mount 被拒绝；
- Redis、TiKV、SQLite 三个后端运行同一套契约。

## 25. 分阶段 PR

（历史交付顺序；PR 6–8 中的 seal/commit/compaction 部分已在 2026-10-17 删除。）

### PR 1：Feature scaffolding 与纯 resolver

- default-off feature；
- `workspace_overlay` module；
- ID/model/error；
- LayerExtent/Hole resolver；
- digest canonical encoder；
- unit/property/benchmark；
- 不改 schema、mount、writer、CLI。

### PR 2：WorkspaceStore 与 SQLite 参考实现

- `ws_v1_*` migrations；
- WorkspaceStore database 实现；
- HeadGuard transaction；
- root volume/workspace 初始化；
- store contract/crash tests；
- 不接 FUSE。

### PR 3：WorkspaceMetaLayer namespace

- stat/lookup/readdir；
- create/unlink/rmdir/rename/link/symlink；
- setattr/xattr/ACL/locks；
- cache/version invalidation；
- namespace POSIX tests；
- 数据仍不接入。

### PR 4：数据读写

- `chunk::read_plan`；
- Backend workspace provider seam；
- FileReader workspace branch；
- WorkspaceMetaLayer write/extent；
- truncate/punch-hole；
- block sharing和写放大 tests；
- default-flat performance comparison。

### PR 5：Mount 与 lease

- volume marker；
- mount-time format 分流；
- lease/heartbeat/fencing；
- writable mount singleton；
- CLI mount flag；
- recovery cleanup。

### PR 6：Fork/discard

- fork from exact revision/discard；
- control CLI/API。

### PR 7：Diff

- path diff。

### PR 8：GC/observability

- layer/slice mark-and-sweep；
- orphan GC；
- metrics/inspect；
- performance gates。

### PR 9：Redis/TiKV 生产 backend

- 独立 `ws:v1:*` prefix；
- backend-neutral KV store；
- Redis binary-safe Lua CAS、cluster hash tag 和 server-time lease；
- TiKV pessimistic transaction、prefix range scan 和冲突重试；
- CLI/mount backend 分流与 namespace 校验；
- Redis/TiKV 双实例并发 contract；
- Redis/TiKV 双 mount isolation、xfstests、pjdfstest gate。

etcd 等其他 backend 必须在独立 PR 中实现；未实现时明确拒绝 workspace volume。

## 26. Definition of Done

Workspace Overlay v1 只有满足以下全部条件才可声明可用：

1. default-flat 所有现有 correctness/performance gate 无回归；
2. feature、schema、cache、task 和 object compatibility 契约全部通过；
3. 两个 workspace 在核心 POSIX mutation 上完全隔离；
4. partial write 只产生修改范围相关的新 slice/block；
5. truncate/hole 不暴露 lower data；
6. fork 与 packed publication crash test 不产生半提交 view；
7. fork 不遍历 base inode/slice/object；
8. stale writer/lease 被 fencing；
9. discard/GC 不删除可达 object；
10. 任何路径都不在他人 live lease 下改变 head（§15.3）；
11. xfstests、pjdfstest 和现有 Rust workspace gate 通过；
12. flat-only 与 workspace-enabled binary 的部署错误均 fail-closed。
13. Redis 和 TiKV 分别通过两个独立 store instance 的 CAS 并发 contract；
14. Redis/TiKV 上两个 sibling workspace 同时挂载时共享不可变 lower 且 mutation 互不可见；
15. 拓扑事务检查集等于读取集、不触碰无关 workspace 的键，并发心跳不阻塞其他 workspace 的
    拓扑操作；
16. v1 目录迁移可续跑且实体完整，迁移完成前 mount 被拒绝，迁移后旧版本二进制 fail-closed。
