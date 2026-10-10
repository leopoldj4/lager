//! Local disk space for receipts and cache reconstruction.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Disk space reservations: work is rejected before the disk becomes full.
pub struct DiskReservations {
    dir: PathBuf,
    min_free: u64,
    reserved: Mutex<u64>,
}

pub struct DiskReservation {
    owner: Arc<DiskReservations>,
    bytes: u64,
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        *self.owner.reserved.lock().unwrap() -= self.bytes;
    }
}

impl DiskReservations {
    pub fn new(dir: &Path, min_free: u64) -> Arc<Self> {
        Arc::new(Self {
            dir: dir.to_path_buf(),
            min_free,
            reserved: Mutex::new(0),
        })
    }

    pub fn reserve(self: &Arc<Self>, bytes: u64) -> Result<DiskReservation, String> {
        let free = free_space(&self.dir).map_err(|e| format!("cannot check disk space: {e}"))?;
        let mut reserved = self.reserved.lock().unwrap();
        // A reservation larger than the free space fails even without a minimum.
        let left = free
            .checked_sub(*reserved)
            .and_then(|f| f.checked_sub(bytes));
        if left.is_none_or(|l| l < self.min_free) {
            return Err("insufficient disk space on the server".into());
        }
        *reserved += bytes;
        Ok(DiskReservation {
            owner: self.clone(),
            bytes,
        })
    }
}

pub fn free_space(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` is a valid out pointer.
    let r = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)]
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}
