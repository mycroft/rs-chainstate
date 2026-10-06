//! Guards against touching a live chainstate.
//!
//! Opening a LevelDB directory is NOT read-only: recovery replays the .log
//! into a new table, rewrites the MANIFEST and deletes obsolete files. Doing
//! that on bitcoind's chainstate can corrupt it. We therefore only ever open
//! snapshots created by `snapshot`, which carry a marker file.
//!
//! Note: rusty-leveldb locks with flock(2) while bitcoind's LevelDB uses
//! fcntl(2) locks; on Linux they do not conflict, so rusty-leveldb alone
//! would happily open a live database. We probe the fcntl lock ourselves.

use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub const SNAPSHOT_MARKER: &str = ".rs-chainstate-snapshot";

/// Returns the pid holding the LevelDB fcntl lock on `dir/LOCK`, if any.
/// Only queries the lock (F_GETLK), never takes it.
pub fn lock_holder(dir: &Path) -> Result<Option<i32>> {
    let lock = dir.join("LOCK");
    let f = match File::open(&lock) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("opening {}", lock.display())),
    };
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    // SAFETY: valid fd and a properly initialized flock struct.
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut fl) } == -1 {
        return Err(std::io::Error::last_os_error()).context("fcntl(F_GETLK)");
    }
    Ok((fl.l_type != libc::F_UNLCK as _).then_some(fl.l_pid))
}

/// Copies a chainstate directory to `dst` without opening it as a database.
pub fn snapshot(src: &Path, dst: &Path, allow_live: bool) -> Result<()> {
    if !src.join("CURRENT").is_file() {
        bail!("{} does not look like a LevelDB directory", src.display());
    }
    if dst.exists() {
        bail!("{} already exists, refusing to overwrite", dst.display());
    }
    if let Some(pid) = lock_holder(src)? {
        if !allow_live {
            bail!(
                "{} is locked by pid {pid} (bitcoind running?). Stop it first for a \
                 consistent snapshot, or pass --allow-live (copy may be inconsistent)",
                src.display()
            );
        }
        eprintln!("warning: copying a live chainstate (pid {pid}), snapshot may be inconsistent");
    }

    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "LOCK" || !entry.file_type()?.is_file() {
            continue;
        }
        fs::copy(entry.path(), dst.join(&name))
            .with_context(|| format!("copying {}", entry.path().display()))?;
    }
    fs::write(
        dst.join(SNAPSHOT_MARKER),
        format!("snapshot of {}\n", src.display()),
    )?;
    Ok(())
}

/// Ensures `dir` is safe to open with a (writing) LevelDB implementation.
pub fn check_openable(dir: &Path, force: bool) -> Result<()> {
    if let Some(pid) = lock_holder(dir)? {
        bail!("{} is locked by pid {pid}, refusing to open", dir.display());
    }
    if !force && !dir.join(SNAPSHOT_MARKER).exists() {
        bail!(
            "{} is not a snapshot (no {SNAPSHOT_MARKER}). Opening it would modify it; \
             run `snapshot` first, or pass --force if it really is a disposable copy",
            dir.display()
        );
    }
    Ok(())
}
