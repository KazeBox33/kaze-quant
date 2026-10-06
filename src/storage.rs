//! 发布报告与锁：所有协作写入者必须使用相同锁；文件系统不支持时直接失败。
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub fn lock_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()
        .map_err(|e| io::Error::other(format!("exclusive lock unavailable: {e}")))?;
    Ok(file)
}
pub fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}
/// 临时文件必须全新；失败保留 .partial 供诊断，不将其当作完成结果。
pub fn publish_new(path: &Path, data: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "report already exists",
        ));
    }
    let mut partial_name = path.as_os_str().to_owned();
    partial_name.push(".partial");
    let partial = std::path::PathBuf::from(partial_name);
    let mut file = File::create_new(&partial)?;
    file.write_all(data)?;
    file.sync_all()?;
    // hard_link 在目标已存在时失败，避免 exists + rename 覆盖并发写入的结果。
    fs::hard_link(&partial, path)?;
    sync_parent(path)?;
    fs::remove_file(partial)?;
    sync_parent(path)
}
