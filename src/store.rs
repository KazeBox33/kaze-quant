//! 事务提交后才发布候选状态和回执；SQLite WAL 承担跨进程崩溃恢复。
//! 热状态只包含有限订单与策略窗口，命令去重和审计历史保留在磁盘。
use crate::config::PaperConfig;
use crate::engine::MAX_LIFETIME_ORDERS;
use crate::journal::{digest, file_hash, hex};
use crate::paper::{Envelope, PaperError, PaperRuntime, PaperSnapshot, Receipt};
use crate::storage::{lock_file, sync_parent};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_STATE: usize = 64 * 1024 * 1024;
const MAX_COMMAND: usize = 8192;
// 条件批量触发需要更大单回执；整批仍限制编码预算，不能随max_batch无界放大。
const MAX_RECEIPT: usize = 8 * 1024 * 1024;
const MAX_BATCH_RECEIPTS: usize = 16 * 1024 * 1024;
type AuditRow = (u64, Vec<u8>, Vec<u8>, Vec<u8>);
const REVISION: &str = "kaze-sql-v2-conditional";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreOptions {
    pub terminal_retention: usize,
    pub max_batch: usize,
    pub max_commands: u64,
    pub max_database_bytes: u64,
    pub max_wal_bytes: u64,
}
impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            terminal_retention: 64,
            max_batch: 256,
            max_commands: MAX_LIFETIME_ORDERS,
            max_database_bytes: 1024 * 1024 * 1024,
            max_wal_bytes: 64 * 1024 * 1024,
        }
    }
}
impl StoreOptions {
    pub fn validate(&self) -> Result<(), PaperError> {
        if self.terminal_retention > 4096
            || !(1..=1024).contains(&self.max_batch)
            || self.max_commands == 0
            || self.max_commands > MAX_LIFETIME_ORDERS
            || !(1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&self.max_database_bytes)
            || !(1024 * 1024..=1024 * 1024 * 1024).contains(&self.max_wal_bytes)
        {
            return Err("invalid store resource limits".into());
        }
        Ok(())
    }
}
impl From<rusqlite::Error> for PaperError {
    fn from(e: rusqlite::Error) -> Self {
        Self(format!("sqlite: {e}"))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    revision: String,
    binary: String,
    config: PaperConfig,
    options: StoreOptions,
}

pub struct SqliteSession {
    conn: Connection,
    _lock: File,
    path: PathBuf,
    runtime: PaperRuntime,
    options: StoreOptions,
    chain: Vec<u8>,
    poisoned: bool,
    pub binary_sha256: String,
    pub config_sha256: String,
}
impl SqliteSession {
    pub fn open(
        path: &Path,
        config: PaperConfig,
        options: StoreOptions,
    ) -> Result<Self, PaperError> {
        Self::open_with_registry(
            path,
            config,
            options,
            std::sync::Arc::new(crate::registry::StrategyRegistry::standard()),
        )
    }
    pub fn open_with_registry(
        path: &Path,
        config: PaperConfig,
        options: StoreOptions,
        registry: std::sync::Arc<crate::registry::StrategyRegistry>,
    ) -> Result<Self, PaperError> {
        config.validate()?;
        options.validate()?;
        // 规范化现有文件或父目录，避免不同路径别名取得两把写入者锁。
        let canonical = if path.exists() {
            path.canonicalize()?
        } else {
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .canonicalize()?
                .join(path.file_name().ok_or("database needs file name")?)
        };
        let path = canonical.as_path();
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock = lock_file(Path::new(&lock_path))?;
        let binary = file_hash(&std::env::current_exe()?)?;
        let config_sha256 = hex(&digest(&serde_json::to_vec(&config)?));
        let manifest = serde_json::to_vec(&Manifest {
            revision: REVISION.into(),
            binary: binary.clone(),
            config: config.clone(),
            options: options.clone(),
        })?;
        let existed = path.exists() && path.metadata()?.len() != 0;
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_millis(100))?;
        conn.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            (MAX_STATE + 1024 * 1024) as i32,
        )?;
        // FULL 每次提交同步 WAL；macOS fullfsync 请求更强的设备刷新。
        let journal: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if journal != "wal" {
            return Err("filesystem does not support SQLite WAL".into());
        }
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON; PRAGMA wal_autocheckpoint=1000; PRAGMA trusted_schema=OFF;")?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let pages = options.max_database_bytes as i64 / page_size;
        let actual: i64 =
            conn.query_row(&format!("PRAGMA max_page_count={pages}"), [], |r| r.get(0))?;
        if actual > pages {
            return Err("existing database exceeds configured page quota".into());
        }
        let mut runtime = PaperRuntime::new_with_registry(config.clone(), registry.clone())?;
        runtime.configure_retention(options.terminal_retention)?;
        let chain = if !existed {
            let state = serde_json::to_vec(&runtime.snapshot()?)?;
            let seed = digest(&manifest).to_vec();
            let tx = conn.transaction()?;
            tx.execute_batch("CREATE TABLE meta(id INTEGER PRIMARY KEY CHECK(id=1),manifest BLOB NOT NULL,state BLOB NOT NULL,state_hash BLOB NOT NULL,seq INTEGER NOT NULL,chain BLOB NOT NULL); CREATE TABLE commands(seq INTEGER PRIMARY KEY,payload BLOB NOT NULL,receipt BLOB NOT NULL,chain BLOB NOT NULL);")?;
            tx.execute(
                "INSERT INTO meta VALUES(1,?1,?2,?3,0,?4)",
                params![manifest, state, digest(&state).as_slice(), seed],
            )?;
            tx.commit()?;
            sync_parent(path)?;
            seed
        } else {
            let (saved, state, state_hash, seq, tail): (Vec<u8>, Vec<u8>, Vec<u8>, u64, Vec<u8>) =
                conn.query_row(
                    "SELECT manifest,state,state_hash,seq,chain FROM meta WHERE id=1",
                    [],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get::<_, i64>(3)? as u64,
                            r.get(4)?,
                        ))
                    },
                )?;
            if saved != manifest {
                return Err("database binary/config/options/revision mismatch".into());
            }
            if state.len() > MAX_STATE
                || digest(&state).as_slice() != state_hash
                || tail.len() != 32
            {
                return Err("checkpoint checksum or bounds invalid".into());
            }
            let snap: PaperSnapshot = serde_json::from_slice(&state)?;
            if snap.retention != Some(options.terminal_retention) {
                return Err("checkpoint retention mismatch".into());
            }
            runtime = PaperRuntime::restore_with_registry(config, snap, registry)?;
            if runtime.processed() != seq || seq > options.max_commands {
                return Err("checkpoint sequence mismatch".into());
            }
            let last: Option<(u64, Vec<u8>)> = conn
                .query_row(
                    "SELECT seq,chain FROM commands ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
                )
                .optional()?;
            if (seq == 0 && (last.is_some() || tail != digest(&manifest)))
                || (seq != 0 && last != Some((seq, tail.clone())))
            {
                return Err("checkpoint and audit tail disagree".into());
            }
            tail
        };
        Ok(Self {
            conn,
            _lock: lock,
            path: path.into(),
            runtime,
            options,
            chain,
            poisoned: false,
            binary_sha256: binary,
            config_sha256,
        })
    }
    pub fn runtime(&self) -> &PaperRuntime {
        &self.runtime
    }
    pub fn chain_sha256(&self) -> String {
        hex(&self.chain)
    }
    pub fn execute_batch(&mut self, inputs: &[Envelope]) -> Result<Vec<Receipt>, PaperError> {
        if self.poisoned {
            return Err("store poisoned after storage failure; close and recover".into());
        }
        if inputs.is_empty() || inputs.len() > self.options.max_batch {
            return Err("batch outside configured capacity".into());
        }
        self.guard_wal()?;
        // 所有修改发生在候选状态。任意校验或 SQL 错误都不会暴露半批次。
        let mut candidate = PaperRuntime::restore_with_registry(
            self.runtime.config().clone(),
            self.runtime.snapshot()?,
            self.runtime.registry(),
        )?;
        let mut chain = self.chain.clone();
        let mut rows: Vec<AuditRow> = Vec::new();
        let mut receipt_budget = 0usize;
        let mut receipts = Vec::with_capacity(inputs.len());
        for input in inputs {
            let payload = serde_json::to_vec(input)?;
            if payload.len() > MAX_COMMAND {
                return Err("command exceeds 8192 bytes".into());
            }
            if input.seq <= candidate.processed() {
                let saved = rows.iter().find(|r| r.0 == input.seq).map(|r| r.1.clone());
                let saved = match saved {
                    Some(s) => Some(s),
                    None => self
                        .conn
                        .query_row(
                            "SELECT payload FROM commands WHERE seq=?1",
                            [input.seq as i64],
                            |r| r.get::<_, Vec<u8>>(0),
                        )
                        .optional()?,
                };
                if saved.as_deref() != Some(payload.as_slice()) {
                    return Err("conflicting retry or missing audit record".into());
                }
                receipts.push(Receipt {
                    seq: input.seq,
                    duplicate: true,
                    notices: Vec::new(),
                });
                continue;
            }
            if input.seq != candidate.processed() + 1 || input.seq > self.options.max_commands {
                return Err("sequence gap or command lifetime capacity".into());
            }
            candidate.validate(&input.command)?;
            let receipt = candidate.apply(input);
            candidate.compact_checkpoint();
            let receipt_bytes = serde_json::to_vec(&receipt)?;
            receipt_budget += receipt_bytes.len();
            if receipt_budget > MAX_BATCH_RECEIPTS {
                return Err("batch receipts exceed durable capacity".into());
            }
            if receipt_bytes.len() > MAX_RECEIPT {
                return Err("receipt exceeds durable capacity".into());
            }
            chain = link(&chain, input.seq, &payload, &receipt_bytes);
            rows.push((input.seq, payload, receipt_bytes, chain.clone()));
            receipts.push(receipt);
        }
        if rows.is_empty() {
            return Ok(receipts);
        }
        candidate.check_invariants()?;
        let state = serde_json::to_vec(&candidate.snapshot()?)?;
        if state.len() > MAX_STATE {
            return Err("checkpoint exceeds 64 MiB".into());
        }
        let committed = (|| -> Result<(), PaperError> {
            let tx = self.conn.transaction()?;
            {
                let mut insert = tx.prepare_cached("INSERT INTO commands VALUES(?1,?2,?3,?4)")?;
                for row in &rows {
                    insert.execute(params![row.0 as i64, row.1, row.2, row.3])?;
                }
            }
            tx.execute(
                "UPDATE meta SET state=?1,state_hash=?2,seq=?3,chain=?4 WHERE id=1",
                params![
                    state,
                    digest(&state).as_slice(),
                    candidate.processed() as i64,
                    chain
                ],
            )?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = committed {
            self.poisoned = true;
            return Err(error);
        }
        self.runtime = candidate;
        self.chain = chain;
        Ok(receipts)
    }
    pub fn storage_stats(&self) -> Result<serde_json::Value, PaperError> {
        let pages: u64 = self
            .conn
            .query_row("PRAGMA page_count", [], |r| Ok(r.get::<_, i64>(0)? as u64))?;
        let page_size: u64 = self
            .conn
            .query_row("PRAGMA page_size", [], |r| Ok(r.get::<_, i64>(0)? as u64))?;
        let mut wal = self.path.as_os_str().to_owned();
        wal.push("-wal");
        Ok(
            serde_json::json!({"logical_database_bytes":pages*page_size,"wal_bytes":std::fs::metadata(Path::new(&wal)).map_or(0,|m|m.len()),"database_limit_bytes":self.options.max_database_bytes,"wal_soft_limit_bytes":self.options.max_wal_bytes,"hot_state_bytes":serde_json::to_vec(&self.runtime.snapshot()?)?.len()}),
        )
    }
    /// 在线备份包含同一事务版本的账本和审计。不可只复制处于 WAL 模式的 .db。
    pub fn backup_new(&self, destination: &Path) -> Result<(), PaperError> {
        if self.poisoned {
            return Err("cannot backup poisoned store; reopen first".into());
        }
        if destination.exists() {
            return Err("backup destination exists".into());
        }
        let mut partial = destination.as_os_str().to_owned();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        let file = File::create_new(&partial)?;
        let mut target = Connection::open(&partial)?;
        {
            let backup = rusqlite::backup::Backup::new(&self.conn, &mut target)?;
            backup.run_to_completion(128, Duration::from_millis(1), None)?;
        }
        drop(target);
        file.sync_all()?;
        std::fs::hard_link(&partial, destination)?;
        sync_parent(destination)?;
        std::fs::remove_file(partial)?;
        sync_parent(destination)?;
        Ok(())
    }
    fn guard_wal(&self) -> Result<(), PaperError> {
        let mut name = self.path.as_os_str().to_owned();
        name.push("-wal");
        let bytes = std::fs::metadata(Path::new(&name)).map_or(0, |m| m.len());
        if bytes >= self.options.max_wal_bytes {
            let (busy, _, _): (u32, u32, u32) =
                self.conn
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?;
            if busy != 0 {
                return Err(
                    "WAL capacity blocked by reader; retry after reader releases snapshot".into(),
                );
            }
        }
        // 一批最多增长状态与命令上限，WAL 配额允许这一批的有界超调。
        Ok(())
    }
    /// 显式扫描全部历史，验证链、逐事件回执与最终检查点；启动不隐含 O(history) 扫描。
    pub fn verify_full(&self) -> Result<u64, PaperError> {
        let manifest: Vec<u8> =
            self.conn
                .query_row("SELECT manifest FROM meta WHERE id=1", [], |r| r.get(0))?;
        let mut chain = digest(&manifest).to_vec();
        let mut runtime = PaperRuntime::new_with_registry(
            self.runtime.config().clone(),
            self.runtime.registry(),
        )?;
        runtime.configure_retention(self.options.terminal_retention)?;
        let mut stmt = self
            .conn
            .prepare("SELECT seq,payload,receipt,chain FROM commands ORDER BY seq")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let seq = row.get::<_, i64>(0)? as u64;
            let payload: Vec<u8> = row.get(1)?;
            let receipt: Vec<u8> = row.get(2)?;
            let saved_chain: Vec<u8> = row.get(3)?;
            if payload.len() > MAX_COMMAND
                || receipt.len() > MAX_RECEIPT
                || seq != runtime.processed() + 1
            {
                return Err("audit record bounds or sequence invalid".into());
            }
            let input: Envelope = serde_json::from_slice(&payload)?;
            if input.seq != seq {
                return Err("audit payload sequence mismatch".into());
            }
            runtime.validate(&input.command)?;
            let expected = runtime.apply(&input);
            runtime.compact_checkpoint();
            if serde_json::to_vec(&expected)? != receipt {
                return Err("audit receipt mismatch".into());
            }
            chain = link(&chain, seq, &payload, &receipt);
            if chain != saved_chain {
                return Err("audit chain mismatch".into());
            }
        }
        if chain != self.chain
            || serde_json::to_vec(&runtime.snapshot()?)?
                != serde_json::to_vec(&self.runtime.snapshot()?)?
        {
            return Err("audit and checkpoint state disagree".into());
        }
        Ok(runtime.processed())
    }
}
fn link(previous: &[u8], seq: u64, payload: &[u8], receipt: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(104);
    bytes.extend_from_slice(previous);
    bytes.extend_from_slice(&seq.to_le_bytes());
    bytes.extend_from_slice(&digest(payload));
    bytes.extend_from_slice(&digest(receipt));
    digest(&bytes).to_vec()
}
