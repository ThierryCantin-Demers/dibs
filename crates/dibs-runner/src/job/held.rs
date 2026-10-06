use crate::lock::LockDir;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Write as _},
    os::unix::ffi::OsStrExt as _,
    path::PathBuf,
    sync::Arc,
};

/// `hold.<pid>`, the fifo a hold's job reads the status of its caller's command from. Being a
/// fifo is also how status tells a hold, which waits on purpose, from a job that is idle.
#[derive(Debug, Clone)]
pub struct HoldFifo {
    pub path: PathBuf,
    /// Open for reading too, so a release written before the job opens the fifo waits in it
    /// rather than finding nobody to take it.
    fifo: Arc<File>,
}

impl HoldFifo {
    pub fn make(dir: &LockDir, pid: u32) -> io::Result<HoldFifo> {
        let path = dir.file("hold", pid);
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: mkfifo only reads the path.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let fifo = OpenOptions::new().read(true).write(true).open(&path)?;
        Ok(HoldFifo {
            path,
            fifo: Arc::new(fifo),
        })
    }

    /// What the job runs: it ends with the status the caller sends.
    pub fn command(&self) -> String {
        let path = self.path.display().to_string().replace('\'', r"'\''");
        format!("read -r st < '{path}' && exit \"$st\"")
    }

    /// Hands the job its caller's status.
    pub fn release(&self, status: i32) {
        let _ = (&*self.fifo).write_all(format!("{status}\n").as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read as _};

    #[test]
    fn a_release_sent_before_the_job_reads_waits_for_it() {
        let dir = std::env::temp_dir().join(format!("dibs-held.{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let held = HoldFifo::make(&LockDir { path: dir.clone() }, 1).unwrap();
        held.release(3);
        let mut said = String::new();
        let mut reader = OpenOptions::new().read(true).open(&held.path).unwrap();
        let mut byte = [0u8; 1];
        while !said.ends_with('\n') && reader.read(&mut byte).unwrap() == 1 {
            said.push(byte[0] as char);
        }
        assert_eq!(said, "3\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
