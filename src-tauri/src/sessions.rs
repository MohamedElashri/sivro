use crate::{
    adapters::{self, Adapter, Interrupt, TableSource},
    api::*,
    dataset::FormatKind,
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
    registry::{self, Fingerprint},
    scratch::{ScratchRoot, SessionScratch},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};

fn lock<T>(mutex: &Mutex<T>) -> AppResult<std::sync::MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| AppError::internal())
}

struct Catalog {
    descriptor: DatasetDescriptor,
    fingerprint: Arc<Fingerprint>,
}
struct Active {
    id: String,
    interrupt: Option<Interrupt>,
}

struct Session {
    _slot: tokio::sync::OwnedSemaphorePermit,
    id: String,
    path: PathBuf,
    options: OpenOptions,
    format: FormatKind,
    delimited: Option<crate::adapters::delimited::FrozenDelimited>,
    generation: AtomicU64,
    closed: AtomicBool,
    refreshing: AtomicBool,
    stale: AtomicBool,
    lane: Arc<AsyncMutex<()>>,
    data: Mutex<Option<Adapter>>,
    catalog: Mutex<Option<Catalog>>,
    active: Mutex<Option<Active>>,
}

impl Session {
    fn object_id(&self, generation: u64) -> String {
        format!("object-{}-{generation}", self.id)
    }
    fn check(&self, generation: u64) -> AppResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(AppError::new(
                ErrorCode::InvalidSession,
                "This session is closed",
            ));
        }
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err(AppError::new(
                ErrorCode::InvalidGeneration,
                "This request belongs to an older source generation",
            ));
        }
        if self.refreshing.load(Ordering::SeqCst) {
            return Err(AppError::new(
                ErrorCode::Busy,
                "The source is being refreshed",
            ));
        }
        if self.stale.load(Ordering::SeqCst) {
            return Err(AppError::new(
                ErrorCode::StaleDataset,
                "The source changed. Refresh before reading more data.",
            ));
        }
        Ok(())
    }
    fn check_object(&self, generation: u64, object_id: &str) -> AppResult<()> {
        self.check(generation)?;
        if object_id != self.object_id(generation) {
            return Err(AppError::new(
                ErrorCode::InvalidObject,
                "That object does not belong to this session generation",
            ));
        }
        Ok(())
    }
}

struct ActiveLease {
    session: Arc<Session>,
    id: String,
}
impl Drop for ActiveLease {
    fn drop(&mut self) {
        if let Ok(mut active) = self.session.active.lock() {
            if active.as_ref().is_some_and(|a| a.id == self.id) {
                *active = None;
            }
        }
    }
}

struct Operation {
    id: String,
    session_id: String,
    generation: u64,
    kind: OperationKind,
    session: Weak<Session>,
    claimed: AtomicBool,
    committed: AtomicBool,
    cancelled: Mutex<Option<AppError>>,
    notify: Notify,
}

pub struct OperationContext {
    operation: Arc<Operation>,
    session: Arc<Session>,
    owner: Weak<AppState>,
}
impl OperationContext {
    pub fn check(&self) -> AppResult<()> {
        if let Some(reason) = lock(&self.operation.cancelled)?.clone() {
            return Err(reason);
        }
        self.session.check(self.operation.generation)
    }
    pub fn cancel(&self, reason: AppError) {
        cancel(&self.operation, reason);
    }
    /// Linearizes publication against cancellation. A cancel arriving after
    /// successful publication reports finished, rather than interrupting a new job.
    pub fn publish<T>(&self, publish: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
        let _active = lock(&self.session.active)?;
        self.check()?;
        let result = publish()?;
        self.operation.committed.store(true, Ordering::SeqCst);
        Ok(result)
    }
}
impl Drop for OperationContext {
    fn drop(&mut self) {
        if let Ok(mut active) = self.session.active.lock() {
            if active.as_ref().is_some_and(|a| a.id == self.operation.id) {
                *active = None;
            }
        }
        if let Some(owner) = self.owner.upgrade() {
            if let Ok(mut operations) = owner.operations.lock() {
                operations.remove(&self.operation.id);
            }
        }
    }
}

fn cancel(op: &Arc<Operation>, reason: AppError) -> String {
    if let Some(session) = op.session.upgrade() {
        if let Ok(active) = session.active.lock() {
            if op.committed.load(Ordering::SeqCst) {
                return "finished".into();
            }
            if let Ok(mut error) = op.cancelled.lock() {
                if error.is_none() {
                    *error = Some(reason);
                }
            }
            op.notify.notify_one();
            if let Some(active) = active.as_ref().filter(|active| active.id == op.id) {
                if let Some(interrupt) = &active.interrupt {
                    interrupt.interrupt();
                }
                return "cancel_pending".into();
            }
        }
    }
    "cancelled".into()
}

pub struct AppState {
    pub policy: ResourcePolicy,
    nonce: String,
    sequence: AtomicU64,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    operations: Mutex<HashMap<String, Arc<Operation>>>,
    previews: Mutex<HashMap<String, crate::adapters::delimited::PreparedDelimited>>,
    permits: Arc<Semaphore>,
    slots: Arc<Semaphore>,
    pub scratch: Arc<ScratchRoot>,
    shutting_down: AtomicBool,
}

impl AppState {
    pub fn new(scratch_path: PathBuf, policy: ResourcePolicy) -> AppResult<Arc<Self>> {
        policy.validate()?;
        let nonce = format!(
            "{:x}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| AppError::internal())?
                .as_nanos(),
            std::process::id()
        );
        let scratch = ScratchRoot::create(&scratch_path, &nonce)?;
        Ok(Arc::new(Self {
            permits: Arc::new(Semaphore::new(policy.expensive_operations)),
            slots: Arc::new(Semaphore::new(policy.max_sessions)),
            policy,
            nonce,
            sequence: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            previews: Mutex::new(HashMap::new()),
            scratch,
            shutting_down: AtomicBool::new(false),
        }))
    }
    fn id(&self, prefix: &str) -> String {
        format!(
            "{prefix}-{}-{:x}",
            self.nonce,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
    fn session(&self, id: &str) -> AppResult<Arc<Session>> {
        if id.len() > 128 || self.shutting_down.load(Ordering::SeqCst) {
            return Err(AppError::new(
                ErrorCode::InvalidSession,
                "Session is unavailable",
            ));
        }
        lock(&self.sessions)?.get(id).cloned().ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidSession,
                "This session is closed or unknown",
            )
        })
    }

    pub async fn open(
        self: &Arc<Self>,
        path: String,
        options: OpenOptions,
    ) -> AppResult<DatasetDescriptor> {
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::limit("Close a file before opening more sessions"))?;
        if lock(&self.sessions)?.len() >= self.policy.max_sessions {
            return Err(AppError::limit("Close a file before opening more sessions"));
        }
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::internal())?;
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if owner.shutting_down.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    ErrorCode::InvalidSession,
                    "The application is closing",
                ));
            }
            let path = registry::resolve(&path, &options)?;
            let format = registry::effective_format(&path, &options)?;
            let id = owner.id("session");
            let scratch =
                SessionScratch::create(owner.scratch.clone(), &format!("{id}-1"), &owner.policy)?;
            let delimited = if matches!(format, FormatKind::Csv | FormatKind::Tsv) {
                let token = options.preview_token.as_ref().ok_or_else(|| {
                    AppError::invalid("Preview and confirm parsing settings before opening CSV/TSV")
                })?;
                let prepared = lock(&owner.previews)?.remove(token).ok_or_else(|| {
                    AppError::invalid("This preview expired; preview the file again")
                })?;
                if prepared.path != path || prepared.created.elapsed().as_secs() > 300 {
                    return Err(AppError::invalid(
                        "Preview does not match this file or expired",
                    ));
                }
                prepared.fingerprint.check(&path)?;
                Some(prepared.frozen)
            } else {
                None
            };
            let adapter = adapters::open(
                &path,
                format,
                delimited.clone(),
                &options,
                format!("object-{id}-1"),
                scratch,
                &owner.policy,
            )?;
            let descriptor = adapter.descriptor(&id, 1, &owner.policy);
            let catalog = Catalog {
                descriptor: descriptor.clone(),
                fingerprint: adapter.fingerprint().clone(),
            };
            let session = Arc::new(Session {
                _slot: slot,
                id: id.clone(),
                path,
                options,
                format,
                delimited,
                generation: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                refreshing: AtomicBool::new(false),
                stale: AtomicBool::new(false),
                lane: Arc::new(AsyncMutex::new(())),
                data: Mutex::new(Some(adapter)),
                catalog: Mutex::new(Some(catalog)),
                active: Mutex::new(None),
            });
            let mut sessions = lock(&owner.sessions)?;
            if sessions.len() >= owner.policy.max_sessions {
                return Err(AppError::limit(
                    "Session limit reached; close a file and retry",
                ));
            }
            if owner.shutting_down.load(Ordering::SeqCst) {
                return Err(AppError::new(
                    ErrorCode::InvalidSession,
                    "The application is closing",
                ));
            }
            sessions.insert(id, session);
            Ok(descriptor)
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    pub async fn preview_delimited(
        self: &Arc<Self>,
        path: String,
        options: crate::adapters::delimited::DelimitedOptions,
    ) -> AppResult<crate::adapters::delimited::DelimitedPreview> {
        let permit = self.admit().await?;
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = registry::resolve(&path, &OpenOptions::default())?;
            let format = registry::extension_format(&path)
                .ok_or_else(|| AppError::invalid("Choose CSV or TSV"))?;
            if !matches!(
                format,
                crate::dataset::FormatKind::Csv | crate::dataset::FormatKind::Tsv
            ) {
                return Err(AppError::invalid("Choose CSV or TSV for parsing preview"));
            }
            let token = owner.id("preview");
            let scratch = SessionScratch::create(owner.scratch.clone(), &token, &owner.policy)?;
            let (prepared, preview) = crate::adapters::delimited::DelimitedAdapter::preview(
                &path,
                format,
                options,
                token.clone(),
                scratch,
                &owner.policy,
            )?;
            let mut previews = lock(&owner.previews)?;
            previews.retain(|_, p| p.created.elapsed().as_secs() <= 300);
            if previews.len() >= owner.policy.max_sessions {
                return Err(AppError::limit(
                    "Too many pending previews; retry after five minutes",
                ));
            }
            previews.insert(token, prepared);
            Ok(preview)
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    /// Lists user tables and views over a read-only connection.
    pub async fn inspect_database(self: &Arc<Self>, path: String) -> AppResult<DatabaseInfo> {
        let permit = self.admit().await?;
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = registry::resolve(&path, &OpenOptions::default())?;
            if registry::extension_format(&path) != Some(FormatKind::Sqlite) {
                return Err(AppError::invalid("Choose a SQLite database"));
            }
            crate::adapters::sqlite::inspect(&path, &owner.policy)
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    /// Lists sheets after the container preflight; nothing is staged.
    pub async fn inspect_workbook(self: &Arc<Self>, path: String) -> AppResult<WorkbookInfo> {
        let permit = self.admit().await?;
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = registry::resolve(&path, &OpenOptions::default())?;
            if registry::extension_format(&path) != Some(FormatKind::Excel) {
                return Err(AppError::invalid("Choose an Excel workbook"));
            }
            crate::adapters::excel::inspect(&path, &owner.policy)
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    pub async fn inspect_hdf5(
        self: &Arc<Self>,
        path: String,
        group: String,
    ) -> AppResult<Hdf5Info> {
        let permit = self.admit().await?;
        let owner = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = registry::resolve(&path, &OpenOptions::default())?;
            if registry::extension_format(&path) != Some(FormatKind::Hdf5) {
                return Err(AppError::invalid("Choose an HDF5 file"));
            }
            crate::adapters::hdf5::inspect(&path, &group, &owner.policy)
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    pub fn discard_preview(&self, token: &str) -> AppResult<()> {
        if token.len() > 128 {
            return Err(AppError::invalid("Invalid preview handle"));
        }
        lock(&self.previews)?.remove(token);
        Ok(())
    }

    pub async fn admit(&self) -> AppResult<tokio::sync::OwnedSemaphorePermit> {
        self.permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::internal())
    }

    pub async fn metadata(
        self: &Arc<Self>,
        session_id: String,
        generation: u64,
        object_id: String,
    ) -> AppResult<DatasetDescriptor> {
        let session = self.session(&session_id)?;
        tokio::task::spawn_blocking(move || {
            session.check_object(generation, &object_id)?;
            let catalog = lock(&session.catalog)?;
            let catalog = catalog.as_ref().ok_or_else(|| {
                AppError::new(
                    ErrorCode::StaleDataset,
                    "Refresh this source before requesting metadata",
                )
            })?;
            if let Err(error) = catalog.fingerprint.check(&session.path) {
                session.stale.store(true, Ordering::SeqCst);
                return Err(error);
            }
            Ok(catalog.descriptor.clone())
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    pub fn begin(
        &self,
        session_id: String,
        generation: u64,
        object_id: String,
        kind: OperationKind,
    ) -> AppResult<OperationHandle> {
        let session = self.session(&session_id)?;
        session.check_object(generation, &object_id)?;
        let mut operations = lock(&self.operations)?;
        session.check_object(generation, &object_id)?;
        if operations.len() >= self.policy.max_pending_operations {
            return Err(AppError::limit(
                "Too many pending operations; cancel or close a file",
            ));
        }
        let id = self.id("operation");
        operations.insert(
            id.clone(),
            Arc::new(Operation {
                id: id.clone(),
                session_id,
                generation,
                kind,
                session: Arc::downgrade(&session),
                claimed: AtomicBool::new(false),
                committed: AtomicBool::new(false),
                cancelled: Mutex::new(None),
                notify: Notify::new(),
            }),
        );
        Ok(OperationHandle { id })
    }

    pub fn cancel_operation(&self, id: &str) -> AppResult<CancelResult> {
        if id.len() > 128 {
            return Err(AppError::invalid("Invalid operation ID"));
        }
        let operation = lock(&self.operations)?.get(id).cloned();
        Ok(CancelResult {
            status: operation
                .map(|op| {
                    cancel(
                        &op,
                        AppError::new(ErrorCode::Cancelled, "Operation cancelled"),
                    )
                })
                .unwrap_or_else(|| "finished".into()),
        })
    }

    /// Runs table work; JSON documents reject it with a structured error.
    pub async fn run<Q, R, F>(
        self: &Arc<Self>,
        request: OperationRequest<Q>,
        kind: OperationKind,
        work: F,
    ) -> AppResult<R>
    where
        Q: Send + 'static,
        R: Send + 'static,
        F: FnOnce(&mut dyn TableSource, u64, Q, &ResourcePolicy, &OperationContext) -> AppResult<R>
            + Send
            + 'static,
    {
        self.run_adapter(request, kind, move |adapter, g, q, p, c| {
            work(adapter.table()?, g, q, p, c)
        })
        .await
    }

    pub async fn run_adapter<Q, R, F>(
        self: &Arc<Self>,
        request: OperationRequest<Q>,
        kind: OperationKind,
        work: F,
    ) -> AppResult<R>
    where
        Q: Send + 'static,
        R: Send + 'static,
        F: FnOnce(&mut Adapter, u64, Q, &ResourcePolicy, &OperationContext) -> AppResult<R>
            + Send
            + 'static,
    {
        if request.operation_id.len() > 128 {
            return Err(AppError::invalid("Operation ID is too long"));
        }
        let session = self.session(&request.session_id)?;
        session.check_object(request.generation, &request.object_id)?;
        let operation = lock(&self.operations)?
            .get(&request.operation_id)
            .cloned()
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::InvalidOperation,
                    "Reserve an operation before requesting work",
                )
            })?;
        if operation.session_id != request.session_id
            || operation.generation != request.generation
            || operation.kind != kind
        {
            return Err(AppError::new(
                ErrorCode::InvalidOperation,
                "Operation handle does not match this request",
            ));
        }
        if operation.claimed.swap(true, Ordering::SeqCst) {
            return Err(AppError::new(
                ErrorCode::InvalidOperation,
                "This operation was already submitted",
            ));
        }
        let context = OperationContext {
            operation: operation.clone(),
            session: session.clone(),
            owner: Arc::downgrade(self),
        };
        context.check()?;
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(self.policy.timeout_ms);
        let lane = tokio::select! {
            _ = operation.notify.notified() => { context.check()?; return Err(AppError::internal()); },
            _ = tokio::time::sleep_until(deadline) => return Err(AppError::new(ErrorCode::Timeout, "Operation timed out while waiting for this session")),
            lane = session.lane.clone().lock_owned() => lane,
        };
        context.check()?;
        let permit = tokio::select! {
            _ = operation.notify.notified() => { context.check()?; return Err(AppError::internal()); },
            _ = tokio::time::sleep_until(deadline) => return Err(AppError::new(ErrorCode::Timeout, "Operation timed out while queued")),
            permit = self.permits.clone().acquire_owned() => permit.map_err(|_| AppError::internal())?,
        };
        let policy = self.policy.clone();
        let mut job = tokio::task::spawn_blocking(move || {
            let (_lane, _permit) = (lane, permit); // native work retains admission even if the async caller goes away
            context.check()?;
            let mut data = lock(&session.data)?;
            let adapter = data
                .as_mut()
                .ok_or_else(|| AppError::new(ErrorCode::InvalidSession, "Session is closed"))?;
            if let Err(error) = adapter.check_source() {
                // A busy or failing check (e.g. a SQLite lock) is not a changed source.
                if error.code == ErrorCode::StaleDataset {
                    session.stale.store(true, Ordering::SeqCst);
                }
                return Err(error);
            }
            {
                let mut active = lock(&session.active)?;
                context.check()?;
                *active = Some(Active {
                    id: operation.id.clone(),
                    interrupt: adapter.interrupt(),
                });
            }
            let _active_lease = ActiveLease {
                session: session.clone(),
                id: operation.id.clone(),
            };
            let result = work(
                adapter,
                request.generation,
                request.query,
                &policy,
                &context,
            );
            context.check()?;
            if !context.operation.committed.load(Ordering::SeqCst) {
                if let Err(error) = adapter.check_source() {
                    if error.code == ErrorCode::StaleDataset {
                        session.stale.store(true, Ordering::SeqCst);
                    }
                    return Err(error);
                }
            }
            result
        });
        match tokio::time::timeout_at(deadline, &mut job).await {
            Ok(result) => result.map_err(|_| AppError::internal())?,
            Err(_) => {
                // Best effort: retain guards and wait for native acknowledgement.
                let operation = lock(&self.operations)?.get(&request.operation_id).cloned();
                if let Some(operation) = operation {
                    cancel(
                        &operation,
                        AppError::new(
                            ErrorCode::Timeout,
                            "The query exceeded its time budget and was interrupted",
                        ),
                    );
                }
                job.await.map_err(|_| AppError::internal())?
            }
        }
    }

    fn cancel_session_operations(&self, session_id: &str) {
        let operations: Vec<_> = self
            .operations
            .lock()
            .map(|ops| {
                ops.values()
                    .filter(|op| op.session_id == session_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for op in operations {
            cancel(
                &op,
                AppError::new(ErrorCode::Cancelled, "The session closed or was refreshed"),
            );
        }
        if let Ok(mut operations) = self.operations.lock() {
            operations
                .retain(|_, op| op.session_id != session_id || op.claimed.load(Ordering::SeqCst));
        }
    }

    pub async fn close(self: &Arc<Self>, session_id: String) -> AppResult<CloseResult> {
        if session_id.len() > 128 {
            return Err(AppError::invalid("Session ID is too long"));
        }
        let session = lock(&self.sessions)?.remove(&session_id);
        let Some(session) = session else {
            return Ok(CloseResult { closed: false });
        };
        session.closed.store(true, Ordering::SeqCst);
        self.cancel_session_operations(&session_id);
        tokio::task::spawn_blocking(move || {
            lock(&session.data)?.take();
            Ok(CloseResult { closed: true })
        })
        .await
        .map_err(|_| AppError::internal())?
    }

    pub async fn refresh(self: &Arc<Self>, session_id: String) -> AppResult<DatasetDescriptor> {
        let session = self.session(&session_id)?;
        if session.refreshing.swap(true, Ordering::SeqCst) {
            return Err(AppError::new(
                ErrorCode::Busy,
                "This source is already refreshing",
            ));
        }
        session.generation.fetch_add(1, Ordering::SeqCst);
        self.cancel_session_operations(&session_id);
        let owner = self.clone();
        // A failed refresh leaves no old connection/header cache usable. The
        // session ID remains available for another explicit refresh attempt.
        let result = async {
            let lane = session.lane.clone().lock_owned().await;
            let permit = owner
                .permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| AppError::internal())?;
            let session = session.clone();
            tokio::task::spawn_blocking(move || {
                let (_lane, _permit) = (lane, permit);
                let generation = session.generation.load(Ordering::SeqCst);
                let mut data = lock(&session.data)?;
                data.take();
                lock(&session.catalog)?.take();
                let path = registry::resolve(
                    session
                        .path
                        .to_str()
                        .ok_or_else(|| AppError::invalid("Invalid source path"))?,
                    &session.options,
                )?;
                let scratch = SessionScratch::create(
                    owner.scratch.clone(),
                    &format!("{}-{generation}", session.id),
                    &owner.policy,
                )?;
                let adapter = adapters::open(
                    &path,
                    session.format,
                    session.delimited.clone(),
                    &session.options,
                    session.object_id(generation),
                    scratch,
                    &owner.policy,
                )?;
                if session.closed.load(Ordering::SeqCst) {
                    return Err(AppError::new(
                        ErrorCode::InvalidSession,
                        "Session closed during refresh",
                    ));
                }
                let descriptor = adapter.descriptor(&session.id, generation, &owner.policy);
                *lock(&session.catalog)? = Some(Catalog {
                    descriptor: descriptor.clone(),
                    fingerprint: adapter.fingerprint().clone(),
                });
                session.stale.store(false, Ordering::SeqCst);
                *data = Some(adapter);
                Ok(descriptor)
            })
            .await
            .map_err(|_| AppError::internal())?
        }
        .await;
        session.refreshing.store(false, Ordering::SeqCst);
        result
    }

    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.permits.close();
        if let Ok(mut previews) = self.previews.lock() {
            previews.clear();
        }
        if let Ok(sessions) = self.sessions.lock() {
            for session in sessions.values() {
                session.closed.store(true, Ordering::SeqCst);
                self.cancel_session_operations(&session.id);
            }
        }
    }

    /// Called by the native exit callback after admission closes. Wait for
    /// owned native jobs before deleting files; interrupts remain best effort.
    /// Drop itself only signals, so a last worker-owned Arc cannot self-join.
    pub fn drain_shutdown(&self) {
        self.shutdown();
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .map(|sessions| sessions.values().cloned().collect())
            .unwrap_or_default();
        for session in sessions {
            if let Ok(mut data) = session.data.lock() {
                data.take();
            }
        }
        while self.permits.available_permits() < self.policy.expensive_operations {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.scratch.cleanup();
    }
}
impl Drop for AppState {
    fn drop(&mut self) {
        self.shutdown();
    }
}
