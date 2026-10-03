//! Files several processes write at once. A rewrite holds the lock beside the file, `<file>.lock`,
//! for its whole read, change and rename, and an append holds it shared, so no line appended
//! meanwhile is lost. Readers take no lock: a rewrite lands by rename, whole.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// A file every writer of which goes through here.
pub struct SharedFile<'a> {
    pub path: &'a Path,
}

impl SharedFile<'_> {
    /// Adds a line, after any rewrite in progress.
    pub fn append(&self, line: &str) -> io::Result<()> {
        let lock = self.lock()?;
        lock.lock_shared()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path)?;
        file.write_all(format!("{line}\n").as_bytes())
    }

    /// Replaces the file with what `change` makes of its text, unless it makes nothing.
    pub fn rewrite(&self, change: impl FnOnce(&str) -> Option<String>) -> io::Result<()> {
        let lock = self.lock()?;
        lock.lock()?;
        let text = match fs::read_to_string(self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let Some(changed) = change(&text) else {
            return Ok(());
        };
        let temporary = self.temporary();
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .and_then(|mut file| file.write_all(changed.as_bytes()))
            .and_then(|()| fs::rename(&temporary, self.path));
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }

    fn lock(&self) -> io::Result<File> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut name = self.path.as_os_str().to_owned();
        name.push(".lock");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(name))
    }

    /// A name beside the file that no other writer, in this process or another, uses.
    fn temporary(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(
            ".{}.{}.new",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        PathBuf::from(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rewrite_replaces_the_file_and_an_append_adds_to_it() {
        let dir = std::env::temp_dir().join(format!("dibs-shared-{}", std::process::id()));
        let path = dir.join("history");
        let file = SharedFile { path: &path };
        file.append("a").unwrap();
        file.append("b").unwrap();
        file.rewrite(|text| Some(text.replace("a\n", ""))).unwrap();
        file.rewrite(|_| None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        let left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left.len(),
            2,
            "the file and its lock, no temporary: {left:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
