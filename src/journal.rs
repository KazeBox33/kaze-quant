//! 同步 write-ahead log：长度有界、SHA-256 链、连续命令号、持久确认、单写入者。
//! 不使用不可信快照反序列化 Engine；从有校验的命令重新构建全部状态。
use crate::config::{EXECUTION_REVISION, PaperConfig, SCHEMA_VERSION};
use crate::paper::{Envelope, PaperError, PaperRuntime, Receipt};
use crate::storage::sync_parent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"KAZEWAL1";
const MAX_FRAME: usize = 65_536;
const MAX_BYTES: u64 = 512 * 1024 * 1024;

pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn file_hash(path: &Path) -> Result<String, PaperError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    execution_revision: String,
    binary_sha256: String,
    config_sha256: String,
    config: PaperConfig,
}

pub struct DurableSession {
    file: File,
    runtime: PaperRuntime,
    previous: [u8; 32],
    /// 每个已提交命令只保存 32 字节身份，历史容量由配置限定。
    command_hashes: Vec<[u8; 32]>,
    bytes: u64,
    poisoned: bool,
    pub recovered_commands: u64,
    pub repaired_tail_bytes: u64,
    pub binary_sha256: String,
    pub config_sha256: String,
}
impl DurableSession {
    pub fn open(path: &Path, config: PaperConfig) -> Result<Self, PaperError> {
        let runtime = PaperRuntime::new(config.clone())?;
        let binary_sha256 = file_hash(&std::env::current_exe()?)?;
        let config_sha256 = hex(&digest(&serde_json::to_vec(&config)?));
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            execution_revision: EXECUTION_REVISION.into(),
            binary_sha256: binary_sha256.clone(),
            config_sha256: config_sha256.clone(),
            config,
        };
        let expected_header = serde_json::to_vec(&manifest)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock()
            .map_err(|e| PaperError(format!("journal is locked or locking unsupported: {e}")))?;
        let bytes = file.metadata()?.len();
        if bytes > MAX_BYTES {
            return Err("journal exceeds 512 MiB; archive and start a new bounded session".into());
        }
        let command_capacity = runtime.config().max_commands;
        let mut this = Self {
            file,
            runtime,
            previous: [0; 32],
            command_hashes: Vec::with_capacity(command_capacity),
            bytes,
            poisoned: false,
            recovered_commands: 0,
            repaired_tail_bytes: 0,
            binary_sha256,
            config_sha256,
        };
        if bytes == 0 {
            this.file.write_all(MAGIC)?;
            this.bytes = MAGIC.len() as u64;
            this.append_frame(&expected_header)?;
            sync_parent(path)?;
        } else {
            let mut magic = [0; 8];
            this.file.read_exact(&mut magic).map_err(|_| {
                PaperError("incomplete journal header; preserve file for diagnosis".into())
            })?;
            if &magic != MAGIC {
                return Err("invalid journal magic".into());
            }
            let header = this.read_frame()?.ok_or("incomplete journal manifest")?;
            let saved: Manifest = serde_json::from_slice(&header)?;
            // 比较规范化配置，接受 JSON 空白差异；不接受引擎/策略/二进制变更。
            if saved.schema_version != SCHEMA_VERSION
                || saved.execution_revision != EXECUTION_REVISION
                || saved.binary_sha256 != this.binary_sha256
                || saved.config_sha256 != this.config_sha256
                || serde_json::to_vec(&saved.config)? != serde_json::to_vec(this.runtime.config())?
            {
                return Err("journal manifest differs from binary/config/execution revision; refuse incompatible recovery".into());
            }
            let mut valid_end = this.file.stream_position()?;
            while let Some(payload) = this.read_frame()? {
                let envelope: Envelope = serde_json::from_slice(&payload)?;
                this.runtime
                    .process(&envelope)
                    .map_err(|e| PaperError(format!("journal semantic validation failed: {e}")))?;
                this.command_hashes
                    .push(digest(&serde_json::to_vec(&envelope)?));
                valid_end = this.file.stream_position()?;
            }
            this.runtime.check_invariants()?;
            this.recovered_commands = this.runtime.processed();
            if valid_end < bytes {
                this.repaired_tail_bytes = bytes - valid_end;
                // 只修复不完整的最后一帧。完整帧校验失败绝不跳过或截断。
                this.file.seek(SeekFrom::Start(valid_end))?;
                let mut tail = Vec::new();
                this.file.read_to_end(&mut tail)?;
                let backup = path.with_extension(format!("torn-{}", hex(&digest(&tail))));
                match File::create_new(&backup) {
                    Ok(mut file) => {
                        file.write_all(&tail)?;
                        file.sync_all()?;
                        sync_parent(&backup)?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        if std::fs::read(&backup)? != tail {
                            return Err("tail backup conflict".into());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
                this.file.set_len(valid_end)?;
                this.file.sync_all()?;
                this.bytes = valid_end;
            }
        }
        this.file.seek(SeekFrom::End(0))?;
        Ok(this)
    }
    pub fn runtime(&self) -> &PaperRuntime {
        &self.runtime
    }
    pub fn high_watermark(&self) -> u64 {
        self.runtime.processed()
    }
    pub fn chain_sha256(&self) -> String {
        hex(&self.previous)
    }
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn execute(&mut self, envelope: Envelope) -> Result<Receipt, PaperError> {
        if self.poisoned {
            return Err(
                "journal write failed; session is poisoned, reopen and recover before retry".into(),
            );
        }
        let payload = serde_json::to_vec(&envelope)?;
        let hash = digest(&payload);
        if envelope.seq > 0 && envelope.seq <= self.high_watermark() {
            if self.command_hashes[(envelope.seq - 1) as usize] != hash {
                return Err("duplicate sequence has conflicting command payload".into());
            }
            return Ok(Receipt {
                seq: envelope.seq,
                duplicate: true,
                notices: Vec::new(),
            });
        }
        if envelope.seq != self.high_watermark() + 1 {
            return Err("command sequence must be contiguous and start at 1".into());
        }
        if self.command_hashes.len() >= self.runtime.config().max_commands {
            return Err("command capacity reached; archive and start a new session".into());
        }
        self.runtime.validate(&envelope.command)?;
        // 写入/同步失败时禁止任何后续命令；状态不会先于 WAL 修改。
        if let Err(e) = self.append_frame(&payload) {
            self.poisoned = true;
            return Err(e);
        }
        let receipt = self.runtime.apply(&envelope);
        self.command_hashes.push(hash);
        Ok(receipt)
    }
    fn append_frame(&mut self, payload: &[u8]) -> Result<(), PaperError> {
        if payload.len() > MAX_FRAME {
            return Err("journal frame exceeds 64 KiB".into());
        }
        let length = (payload.len() as u32).to_le_bytes();
        let added = payload.len() as u64 + 36;
        if self.bytes + added > MAX_BYTES {
            return Err("journal capacity reached (512 MiB)".into());
        }
        let mut hasher = Sha256::new();
        hasher.update(self.previous);
        hasher.update(length);
        hasher.update(payload);
        let hash: [u8; 32] = hasher.finalize().into();
        self.file.write_all(&length)?;
        self.file.write_all(payload)?;
        self.file.write_all(&hash)?;
        self.file.sync_all()?;
        self.bytes += added;
        self.previous = hash;
        Ok(())
    }
    fn read_frame(&mut self) -> Result<Option<Vec<u8>>, PaperError> {
        let position = self.file.stream_position()?;
        let remaining = self.bytes - position;
        if remaining < 4 {
            return Ok(None);
        }
        let mut length = [0; 4];
        self.file.read_exact(&mut length)?;
        let n = u32::from_le_bytes(length) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err("invalid journal frame length; file quarantined".into());
        }
        if remaining < n as u64 + 36 {
            return Ok(None);
        }
        let mut payload = vec![0; n];
        self.file.read_exact(&mut payload)?;
        let mut expected = [0; 32];
        self.file.read_exact(&mut expected)?;
        let mut hasher = Sha256::new();
        hasher.update(self.previous);
        hasher.update(length);
        hasher.update(&payload);
        let actual: [u8; 32] = hasher.finalize().into();
        if expected != actual {
            return Err("journal checksum mismatch; file quarantined, do not trade".into());
        }
        self.previous = actual;
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod fault_tests {
    use super::*;
    #[test]
    fn failed_write_does_not_apply_and_permanently_poisons_writer() {
        let path = std::env::temp_dir().join(format!("kaze-write-fault-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let config: PaperConfig =
            serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
        let mut session = DurableSession::open(&path, config).unwrap();
        let readonly = File::open(&path).unwrap();
        // 仅测试注入只读句柄，不依赖 /dev/full 或 root 权限行为。
        let locked_writer = std::mem::replace(&mut session.file, readonly);
        let command = Envelope {
            seq: 1,
            command: crate::paper::Command::Halt {},
        };
        let before = session.runtime().report();
        assert!(session.execute(command.clone()).is_err());
        assert_eq!(session.runtime().report(), before);
        assert!(session.poisoned());
        session.file = locked_writer;
        assert!(session.execute(command).is_err());
        drop(session);
        std::fs::remove_file(path).unwrap();
    }
}
