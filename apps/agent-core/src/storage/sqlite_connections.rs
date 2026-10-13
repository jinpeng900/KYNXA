use super::{ContractError, cancellation, sqlite_lexical, sqlite_store};
use crate::file_identity::HandleIdentity;
use rusqlite::{Connection, OpenFlags};
use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

const MAX_DATABASE_POOLS: usize = 16;
const MAX_READ_CONNECTIONS: usize = 8;
static POOLS: OnceLock<Mutex<HashMap<PathBuf, Arc<ConnectionPool>>>> = OnceLock::new();
static IDENTITY_CHECKS: AtomicU64 = AtomicU64::new(0);
static PATH_INSPECTIONS: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static THREAD_BOUNDARY_COUNTS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}
pub(crate) fn boundary_counts() -> (u64, u64) {
    THREAD_BOUNDARY_COUNTS.with(Cell::get)
}
pub(crate) fn boundary_audit(before: (u64, u64)) -> serde_json::Value {
    let after = boundary_counts();
    serde_json::json!({"identityHandleChecks":after.0.saturating_sub(before.0),
        "managedPathInspections":after.1.saturating_sub(before.1),"unit":"logical-boundary-checks"})
}

struct PooledConnection {
    connection: Connection,
    lexical: Option<Arc<RwLock<sqlite_lexical::RankingContext>>>,
}

#[derive(Default)]
struct ConnectionGroup {
    idle: Vec<PooledConnection>,
    handles: usize,
    opened: u64,
    reused: u64,
}

#[derive(Default)]
struct PoolState {
    reads: ConnectionGroup,
    writes: ConnectionGroup,
    retired: bool,
}
impl PoolState {
    fn group(&mut self, writable: bool) -> &mut ConnectionGroup {
        if writable {
            &mut self.writes
        } else {
            &mut self.reads
        }
    }
    fn retire(&mut self) {
        self.retired = true;
        for group in [&mut self.reads, &mut self.writes] {
            group.handles = group.handles.saturating_sub(group.idle.len());
            group.idle.clear();
        }
    }
}

struct ConnectionPool {
    identity: HandleIdentity,
    state: Mutex<PoolState>,
    available: Condvar,
}

/// A lease owns its handle; the pool lock never covers SQL execution or an active snapshot.
/// 租约独占数据库句柄，连接池锁不覆盖 SQL 执行或活动快照。
pub struct SqliteConnectionLease {
    entry: Option<PooledConnection>,
    pool: Arc<ConnectionPool>,
    writable: bool,
}

impl Deref for SqliteConnectionLease {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.entry.as_ref().expect("active SQLite lease").connection
    }
}

impl SqliteConnectionLease {
    pub(crate) fn database_identity(&self) -> (u64, u64) {
        (self.pool.identity.volume, self.pool.identity.identity)
    }
    pub(crate) fn lexical_context(
        &mut self,
    ) -> Result<Arc<RwLock<sqlite_lexical::RankingContext>>, ContractError> {
        let entry = self.entry.as_mut().expect("active SQLite lease");
        if entry.lexical.is_none() {
            let context = Arc::new(RwLock::new(sqlite_lexical::RankingContext::default()));
            sqlite_lexical::initialize_connection(&entry.connection, context.clone())?;
            entry.lexical = Some(context);
        }
        Ok(entry
            .lexical
            .as_ref()
            .expect("initialized lexical context")
            .clone())
    }
}

impl Drop for SqliteConnectionLease {
    fn drop(&mut self) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        // Clear request deadlines before cleanup; a cancelled request must not poison its next borrower.
        // 清理前撤除请求截止回调，已取消的请求不能污染下一位借用者。
        entry.connection.progress_handler(0, None::<fn() -> bool>);
        let clean_transaction =
            entry.connection.is_autocommit() || entry.connection.execute_batch("ROLLBACK").is_ok();
        let clean_terms = entry.lexical.is_none()
            || entry
                .connection
                .execute("DELETE FROM temp.retrieval_query_terms", [])
                .is_ok();
        if let Some(context) = &entry.lexical {
            if let Ok(mut ranking) = context.write() {
                *ranking = sqlite_lexical::RankingContext::default();
            }
        }
        if let Ok(mut state) = self.pool.state.lock() {
            let retired = state.retired;
            let group = state.group(self.writable);
            if clean_transaction && clean_terms && !retired {
                group.idle.push(entry);
            } else {
                group.handles = group.handles.saturating_sub(1);
            }
            self.pool.available.notify_all();
        }
    }
}

fn checked_identity(root: &Path, path: &Path) -> Result<HandleIdentity, ContractError> {
    THREAD_BOUNDARY_COUNTS.with(|count| {
        let old = count.get();
        count.set((old.0 + 1, old.1 + 4));
    });
    IDENTITY_CHECKS.fetch_add(1, Ordering::Relaxed);
    PATH_INSPECTIONS.fetch_add(4, Ordering::Relaxed);
    if !sqlite_store::inspect_path(root, path, false, true)? {
        return Err(sqlite_store::failure("index is missing"));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        sqlite_store::inspect_sidecar(
            root,
            Path::new(&format!("{}{suffix}", path.to_string_lossy())),
        )?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000);
    }
    let handle = options.open(path).map_err(sqlite_store::failure)?;
    let identity = crate::file_identity::handle_identity(&handle).map_err(sqlite_store::failure)?;
    if identity.is_link || identity.links != 1 {
        return Err(ContractError::new(
            "Unsafe retrieval file identity. / 检索文件身份包含链接结构。",
            "UNSAFE_RETRIEVAL_PATH",
            409,
        ));
    }
    Ok(identity)
}

fn pool_for(root: &Path, path: &Path) -> Result<Arc<ConnectionPool>, ContractError> {
    let identity = checked_identity(root, path)?;
    let mut pools = POOLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(sqlite_store::failure)?;
    if let Some(pool) = pools.get(path) {
        if pool.identity == identity {
            return Ok(pool.clone());
        }
        // Replacing a database retires old handles instead of querying the unlinked file forever.
        // 数据库文件替换后退休旧句柄，不能继续查询已脱离当前路径的旧文件。
        if let Ok(mut state) = pool.state.lock() {
            state.retire();
        }
        pools.remove(path);
    }
    if pools.len() >= MAX_DATABASE_POOLS {
        let candidate = pools
            .iter()
            .find(|(_, pool)| Arc::strong_count(pool) == 1)
            .map(|(path, _)| path.clone());
        if let Some(candidate) = candidate {
            pools.remove(&candidate);
        } else {
            return Err(ContractError::new(
                "SQLite read pools are busy. / SQLite 读取连接池繁忙。",
                "RETRIEVAL_CONNECTION_CAPACITY",
                503,
            ));
        }
    }
    let pool = Arc::new(ConnectionPool {
        identity,
        state: Mutex::new(PoolState::default()),
        available: Condvar::new(),
    });
    pools.insert(path.to_owned(), pool.clone());
    Ok(pool)
}

/// Each checkout starts a fresh transaction and binds a fresh deadline and cancellation context.
/// 每次借用都启动新事务并绑定新的截止时间与取消上下文，不复用旧快照。
pub fn acquire(
    root: &Path,
    path: &Path,
    timeout_ms: u64,
) -> Result<SqliteConnectionLease, ContractError> {
    acquire_owned(root, path, timeout_ms, false)
}

/// Each database has one reusable writer; unrelated databases and read leases keep running.
/// 每个数据库拥有一个可复用写句柄，独立数据库及读取租约继续并行执行。
pub fn acquire_write(
    root: &Path,
    path: &Path,
    timeout_ms: u64,
) -> Result<SqliteConnectionLease, ContractError> {
    acquire_owned(root, path, timeout_ms, true)
}

fn acquire_owned(
    root: &Path,
    path: &Path,
    timeout_ms: u64,
    writable: bool,
) -> Result<SqliteConnectionLease, ContractError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.clamp(1, 120_000));
    let pool = pool_for(root, path)?;
    let entry = loop {
        sqlite_store::cancelled()?;
        if started.elapsed() >= timeout {
            return Err(ContractError::new(
                "SQLite connection admission timed out. / SQLite 连接准入超时。",
                "RETRIEVAL_SEARCH_TIMEOUT",
                504,
            ));
        }
        let mut state = pool.state.lock().map_err(sqlite_store::failure)?;
        if state.retired {
            return Err(ContractError::new(
                "SQLite index changed during admission. / SQLite 索引在准入期间发生变化。",
                "STALE_RETRIEVAL_SOURCE",
                409,
            ));
        }
        let group = state.group(writable);
        if let Some(entry) = group.idle.pop() {
            group.reused += 1;
            break entry;
        }
        if group.handles < if writable { 1 } else { MAX_READ_CONNECTIONS } {
            group.handles += 1;
            drop(state);
            let opened = (|| {
                let connection = Connection::open_with_flags(
                    path,
                    (if writable {
                        OpenFlags::SQLITE_OPEN_READ_WRITE
                    } else {
                        OpenFlags::SQLITE_OPEN_READ_ONLY
                    }) | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(sqlite_store::failure)?;
                connection
                    .busy_timeout(
                        timeout
                            .saturating_sub(started.elapsed())
                            .min(Duration::from_secs(3)),
                    )
                    .map_err(sqlite_store::failure)?;
                connection
                    .execute_batch(if writable {
                        "PRAGMA foreign_keys=ON;PRAGMA synchronous=FULL;"
                    } else {
                        "PRAGMA foreign_keys=ON;"
                    })
                    .map_err(sqlite_store::failure)?;
                connection.set_prepared_statement_cache_capacity(128);
                if checked_identity(root, path)? != pool.identity {
                    return Err(ContractError::new(
                        "SQLite index was replaced. / SQLite 索引已被替换。",
                        "STALE_RETRIEVAL_SOURCE",
                        409,
                    ));
                }
                Ok(PooledConnection {
                    connection,
                    lexical: None,
                })
            })();
            match opened {
                Ok(entry) => {
                    pool.state
                        .lock()
                        .map_err(sqlite_store::failure)?
                        .group(writable)
                        .opened += 1;
                    break entry;
                }
                Err(error) => {
                    let mut state = pool.state.lock().map_err(sqlite_store::failure)?;
                    let group = state.group(writable);
                    group.handles = group.handles.saturating_sub(1);
                    pool.available.notify_all();
                    return Err(error);
                }
            }
        }
        let _ = pool
            .available
            .wait_timeout(state, Duration::from_millis(10))
            .map_err(sqlite_store::failure)?;
    };
    let lease = SqliteConnectionLease {
        entry: Some(entry),
        pool,
        writable,
    };
    // Reused handles must honor this checkout's deadline during SQLite busy waits as well as SQL progress.
    // 复用句柄同时按本次期限限制SQLite忙等待及SQL执行，不能只缩短借连接排队时间。
    lease
        .busy_timeout(
            timeout
                .saturating_sub(started.elapsed())
                .min(Duration::from_secs(3)),
        )
        .map_err(sqlite_store::failure)?;
    lease.progress_handler(
        256,
        Some(move || cancellation::is_cancelled() || started.elapsed() >= timeout),
    );
    let version: i64 = lease
        .prepare_cached("PRAGMA user_version")
        .map_err(sqlite_store::failure)?
        .query_row([], |row| row.get(0))
        .map_err(sqlite_store::failure)?;
    if version > 5 {
        return Err(ContractError::new(
            "A newer retrieval index exists. / 检索索引由更新版本创建，原文件已保留。",
            "UNSUPPORTED_RETRIEVAL_INDEX_VERSION",
            409,
        ));
    }
    Ok(lease)
}

pub fn close(root: &Path, path: &Path) -> Result<(), ContractError> {
    sqlite_store::inspect_path(root, path, false, true)?;
    if let Some(pools) = POOLS.get() {
        let mut pools = pools.lock().map_err(sqlite_store::failure)?;
        if let Some(pool) = pools.remove(path) {
            let mut state = pool.state.lock().map_err(sqlite_store::failure)?;
            state.retire();
            pool.available.notify_all();
        }
    }
    Ok(())
}

pub fn stats(root: &Path, path: &Path) -> Result<serde_json::Value, ContractError> {
    let pool = pool_for(root, path)?;
    let state = pool.state.lock().map_err(sqlite_store::failure)?;
    Ok(
        serde_json::json!({"maximumSqliteConnectionLeases":MAX_READ_CONNECTIONS,
        "maximumWriteConnections":1,
        "openConnections":state.reads.handles + state.writes.handles,
        "idleConnections":state.reads.idle.len() + state.writes.idle.len(),
        "activeConnections":state.reads.handles.saturating_sub(state.reads.idle.len()) + state.writes.handles.saturating_sub(state.writes.idle.len()),
        "opened":state.reads.opened + state.writes.opened,"reused":state.reads.reused + state.writes.reused,
        "readHandlesOpened":state.reads.opened,"readHandlesReused":state.reads.reused,
        "writeHandlesOpened":state.writes.opened,"writeHandlesReused":state.writes.reused,
        "identityHandleChecks":IDENTITY_CHECKS.load(Ordering::Relaxed),
        "managedPathInspections":PATH_INSPECTIONS.load(Ordering::Relaxed),
        "retired":state.retired}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        path: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kynxa-sqlite-pool-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("fixture.sqlite");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES('first');").unwrap();
            Self { root, path }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            close(&self.root, &self.path).unwrap();
            assert!(self.root.starts_with(std::env::temp_dir()));
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn reused_handles_release_snapshots_and_observe_external_commits() {
        let fixture = Fixture::new();
        {
            let lease = acquire(&fixture.root, &fixture.path, 120_000).unwrap();
            lease.execute_batch("BEGIN").unwrap();
            assert_eq!(
                lease
                    .query_row("SELECT value FROM sample", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "first"
            );
        }
        let writer = Connection::open(&fixture.path).unwrap();
        writer
            .execute("UPDATE sample SET value='second'", [])
            .unwrap();
        {
            let lease = acquire(&fixture.root, &fixture.path, 120_000).unwrap();
            lease.execute_batch("BEGIN").unwrap();
            assert_eq!(
                lease
                    .query_row("SELECT value FROM sample", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "second"
            );
            lease.execute_batch("COMMIT").unwrap();
        }
        let current = stats(&fixture.root, &fixture.path).unwrap();
        assert_eq!(current["opened"], 1);
        assert_eq!(current["reused"], 1);
        assert_eq!(current["activeConnections"], 0);
    }

    #[test]
    fn read_leases_run_independently_and_waiting_admission_is_cancelled() {
        let fixture = Fixture::new();
        let leases: Vec<_> = (0..MAX_READ_CONNECTIONS)
            .map(|_| acquire(&fixture.root, &fixture.path, 120_000).unwrap())
            .collect();
        let id = format!(
            "sqlite-pool-wait:{}",
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        );
        cancellation::retain(&id).unwrap();
        let root = fixture.root.clone();
        let path = fixture.path.clone();
        let task_id = id.clone();
        let waiting = std::thread::spawn(move || {
            let token = cancellation::CancellationLease::acquire(&task_id).unwrap();
            let _context = token.enter();
            acquire(&root, &path, 120_000).err().unwrap().code
        });
        for lease in &leases {
            assert_eq!(
                lease
                    .query_row("SELECT count(*) FROM sample", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        cancellation::cancel(&id).unwrap();
        assert_eq!(waiting.join().unwrap(), "ABORT_ERR");
        cancellation::release(&id).unwrap();
        assert_eq!(
            stats(&fixture.root, &fixture.path).unwrap()["activeConnections"],
            8
        );
        drop(leases);
        assert!(acquire(&fixture.root, &fixture.path, 120_000).is_ok());
    }

    #[test]
    fn expired_query_deadline_does_not_poison_the_next_borrower() {
        let fixture = Fixture::new();
        {
            let lease = acquire(&fixture.root, &fixture.path, 50).unwrap();
            let result = lease.query_row("WITH RECURSIVE numbers(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM numbers WHERE x<10000000) SELECT sum(x) FROM numbers", [], |row| row.get::<_, i64>(0));
            assert!(result.is_err());
        }
        let lease = acquire(&fixture.root, &fixture.path, 120_000).unwrap();
        assert_eq!(
            lease
                .query_row("SELECT value FROM sample", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "first"
        );
    }

    #[test]
    fn writers_reuse_statements_without_blocking_readers_or_other_databases() {
        let fixture = Fixture::new();
        let other = Fixture::new();
        {
            let writer = acquire_write(&fixture.root, &fixture.path, 120_000).unwrap();
            writer.execute_batch("BEGIN IMMEDIATE").unwrap();
            writer
                .prepare_cached("UPDATE sample SET value=?")
                .unwrap()
                .execute(["pending"])
                .unwrap();
            let reader = acquire(&fixture.root, &fixture.path, 120_000).unwrap();
            assert_eq!(
                reader
                    .query_row("SELECT value FROM sample", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "first"
            );
            let independent_writer = acquire_write(&other.root, &other.path, 120_000).unwrap();
            independent_writer
                .execute("UPDATE sample SET value='independent'", [])
                .unwrap();
            // Dropping an unfinished write rolls it back before reusing the same writer handle.
            // 未完成写事务在句柄复用前回滚，其他数据库的写入不受该事务阻塞。
        }
        {
            let writer = acquire_write(&fixture.root, &fixture.path, 120_000).unwrap();
            assert_eq!(
                writer
                    .query_row("SELECT value FROM sample", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "first"
            );
            writer
                .prepare_cached("UPDATE sample SET value=?")
                .unwrap()
                .execute(["committed"])
                .unwrap();
        }
        let state = stats(&fixture.root, &fixture.path).unwrap();
        assert_eq!(state["writeHandlesOpened"], 1);
        assert_eq!(state["writeHandlesReused"], 1);
        let reader = acquire(&fixture.root, &fixture.path, 120_000).unwrap();
        assert_eq!(
            reader
                .query_row("SELECT value FROM sample", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "committed"
        );
    }
}
