//! Opt-in records for one logical chunk during an isolated upload diagnosis.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static DIAGNOSTIC: LazyLock<Option<Mutex<Recorder>>> = LazyLock::new(|| {
    let chunk_id = std::env::var("BREWFS_UPLOAD_DIAGNOSTIC_CHUNK_ID")
        .ok()?
        .parse()
        .ok()?;
    let path = std::env::var_os("BREWFS_UPLOAD_DIAGNOSTIC_LOG")?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .ok()?;
    Some(Mutex::new(Recorder {
        chunk_id,
        file,
        slices: BTreeMap::new(),
        queues: BTreeMap::new(),
        first_failure_recorded: false,
    }))
});

struct Recorder {
    chunk_id: u64,
    file: File,
    slices: BTreeMap<u64, SliceDiagnostic>,
    queues: BTreeMap<(u8, usize), QueueProgress>,
    first_failure_recorded: bool,
}

#[derive(Clone, Copy, Default)]
struct QueueProgress {
    issued: u64,
    granted: u64,
    cancelled: u64,
    waiting: u64,
    max_granted_ticket: u64,
}

fn with_recorder<T>(f: impl FnOnce(&mut Recorder) -> T) -> Option<T> {
    let mut recorder = DIAGNOSTIC.as_ref()?.lock().ok()?;
    Some(f(&mut recorder))
}

impl Recorder {
    fn emit(&mut self, slice: SliceDiagnostic, slice_id: u64, stage: &str, fields: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        if let Err(error) = writeln!(
            self.file,
            "{{\"time_unix_ns\":{now},\"stage\":\"{stage}\",\"chunk_id\":{},\"slice_id\":{slice_id},\"creation_unique\":{},\"slice_offset\":{},\"slice_age_ns\":{}{fields}}}",
            slice.chunk_id,
            slice.creation_unique,
            slice.offset,
            slice.created.elapsed().as_nanos(),
        ) {
            eprintln!("upload diagnostic record failed: {error}");
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SliceDiagnostic {
    chunk_id: u64,
    offset: u64,
    creation_unique: u64,
    created: Instant,
}

impl SliceDiagnostic {
    pub(crate) fn new(
        chunk_id: u64,
        offset: u64,
        creation_unique: u64,
        created: Instant,
    ) -> Option<Self> {
        with_recorder(|recorder| {
            (recorder.chunk_id == chunk_id).then_some(Self {
                chunk_id,
                offset,
                creation_unique,
                created,
            })
        })
        .flatten()
    }

    pub(crate) fn for_failure(
        chunk_id: u64,
        offset: u64,
        creation_unique: u64,
        created: Instant,
    ) -> Option<Self> {
        with_recorder(|recorder| {
            let selected = recorder.chunk_id == chunk_id || !recorder.first_failure_recorded;
            recorder.first_failure_recorded = true;
            selected.then_some(Self {
                chunk_id,
                offset,
                creation_unique,
                created,
            })
        })
        .flatten()
    }

    pub(crate) fn record(self, stage: &str, slice_id: u64) {
        with_recorder(|recorder| recorder.emit(self, slice_id, stage, ""));
    }

    pub(crate) fn register(self, slice_id: u64) {
        with_recorder(|recorder| {
            recorder.slices.insert(slice_id, self);
            recorder.emit(self, slice_id, "slice_id_assigned", "");
        });
    }

    pub(crate) fn state(self, stage: &str, slice_id: u64, fields: &str) {
        with_recorder(|recorder| {
            recorder.emit(self, slice_id, stage, fields);
            if stage == "slice_failed" {
                let queues: Vec<_> = recorder
                    .queues
                    .iter()
                    .map(|(key, value)| (*key, *value))
                    .collect();
                for ((kind, semaphore_id), progress) in queues {
                    recorder.emit(
                        self,
                        slice_id,
                        "queue_snapshot",
                        &queue_fields(kind, semaphore_id, 0, progress, 0),
                    );
                }
            }
        });
    }
}

#[derive(Clone, Copy)]
pub(crate) struct BlockDiagnostic {
    slice: SliceDiagnostic,
    slice_id: u64,
    block_index: u32,
    offset: u64,
    length: u64,
    attempt: u32,
}

impl BlockDiagnostic {
    pub(crate) fn for_key(
        slice_id: u64,
        block_index: u32,
        offset: u64,
        length: u64,
    ) -> Option<Self> {
        with_recorder(|recorder| {
            recorder.slices.get(&slice_id).copied().map(|slice| Self {
                slice,
                slice_id,
                block_index,
                offset,
                length,
                attempt: 0,
            })
        })
        .flatten()
    }

    pub(crate) fn for_object_key(key: &str, length: u64) -> Option<Self> {
        let (slice, block) = key.strip_prefix("chunks-v2/")?.split_once('/')?;
        Self::for_key(slice.parse().ok()?, block.parse().ok()?, 0, length)
    }

    pub(crate) fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    fn emit(self, recorder: &mut Recorder, stage: &str, fields: &str) {
        let fields = format!(
            ",\"block_index\":{},\"block_offset\":{},\"length\":{},\"attempt\":{},\"object_key\":\"chunks-v2/{}/{}\"{fields}",
            self.block_index,
            self.offset,
            self.length,
            self.attempt,
            self.slice_id,
            self.block_index,
        );
        recorder.emit(self.slice, self.slice_id, stage, &fields);
    }

    pub(crate) fn stage(self, stage: &'static str) -> StageGuard {
        with_recorder(|recorder| self.emit(recorder, stage, ",\"transition\":\"begin\""));
        StageGuard {
            context: self,
            stage,
            started: Instant::now(),
            finished: false,
        }
    }
}

pub(crate) struct StageGuard {
    context: BlockDiagnostic,
    stage: &'static str,
    started: Instant,
    finished: bool,
}

impl StageGuard {
    pub(crate) fn finish(mut self, success: bool) {
        self.finished = true;
        with_recorder(|recorder| {
            self.context.emit(
                recorder,
                self.stage,
                &format!(
                    ",\"transition\":\"end\",\"success\":{success},\"elapsed_ns\":{}",
                    self.started.elapsed().as_nanos(),
                ),
            )
        });
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if !self.finished {
            with_recorder(|recorder| {
                self.context.emit(
                    recorder,
                    self.stage,
                    &format!(
                        ",\"transition\":\"cancelled\",\"elapsed_lower_bound_ns\":{}",
                        self.started.elapsed().as_nanos(),
                    ),
                )
            });
        }
    }
}

fn queue_fields(
    kind: u8,
    semaphore_id: usize,
    ticket: u64,
    progress: QueueProgress,
    elapsed: u128,
) -> String {
    format!(
        ",\"queue_kind\":{kind},\"semaphore_id\":{semaphore_id},\"ticket\":{ticket},\"issued\":{},\"granted\":{},\"cancelled\":{},\"waiting\":{},\"max_granted_ticket\":{},\"elapsed_ns\":{elapsed}",
        progress.issued,
        progress.granted,
        progress.cancelled,
        progress.waiting,
        progress.max_granted_ticket,
    )
}

/// Kind 0 is a per-file limit, 1 foreground, and 2 writeback.
pub(crate) struct PermitWait {
    key: (u8, usize),
    ticket: u64,
    context: Option<BlockDiagnostic>,
    started: Instant,
    acquired: bool,
}

impl PermitWait {
    pub(crate) fn begin(
        kind: u8,
        semaphore_id: usize,
        context: Option<BlockDiagnostic>,
    ) -> Option<Self> {
        with_recorder(|recorder| {
            let progress = recorder.queues.entry((kind, semaphore_id)).or_default();
            progress.issued += 1;
            progress.waiting += 1;
            let progress = *progress;
            let ticket = progress.issued;
            if let Some(context) = context {
                context.emit(
                    recorder,
                    "permit_wait",
                    &queue_fields(kind, semaphore_id, ticket, progress, 0),
                );
            }
            Self {
                key: (kind, semaphore_id),
                ticket,
                context,
                started: Instant::now(),
                acquired: false,
            }
        })
    }

    pub(crate) fn acquired(mut self) {
        self.acquired = true;
        with_recorder(|recorder| {
            let progress = recorder
                .queues
                .get_mut(&self.key)
                .expect("diagnostic queue registered");
            progress.granted += 1;
            progress.waiting -= 1;
            progress.max_granted_ticket = progress.max_granted_ticket.max(self.ticket);
            let progress = *progress;
            if let Some(context) = self.context {
                context.emit(
                    recorder,
                    "permit_acquired",
                    &queue_fields(
                        self.key.0,
                        self.key.1,
                        self.ticket,
                        progress,
                        self.started.elapsed().as_nanos(),
                    ),
                );
            }
        });
    }
}

impl Drop for PermitWait {
    fn drop(&mut self) {
        if !self.acquired {
            with_recorder(|recorder| {
                let progress = recorder
                    .queues
                    .get_mut(&self.key)
                    .expect("diagnostic queue registered");
                progress.cancelled += 1;
                progress.waiting -= 1;
                let progress = *progress;
                if let Some(context) = self.context {
                    context.emit(
                        recorder,
                        "permit_cancelled",
                        &queue_fields(
                            self.key.0,
                            self.key.1,
                            self.ticket,
                            progress,
                            self.started.elapsed().as_nanos(),
                        ),
                    );
                }
            });
        }
    }
}
