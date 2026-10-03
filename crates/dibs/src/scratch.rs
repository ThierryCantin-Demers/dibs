//! Files a call writes for a moment and hands to another process: created new, readable by their
//! owner alone, under names nobody can guess, so nothing planted ahead of them is written through.

use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
};

/// How many names are tried before giving up, each a fresh draw.
const ATTEMPTS: u32 = 8;
/// Random bytes in a name: 96 bits, as hex.
const NAME_BYTES: usize = 12;

pub struct ScratchFile;

impl ScratchFile {
    /// `<dir>/<prefix>.<random>`, holding `contents`, with `dir` made if it is missing.
    pub fn create(dir: &Path, prefix: &str, contents: &[u8]) -> io::Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let mut taken = None;
        for _ in 0..ATTEMPTS {
            let path = dir.join(format!("{prefix}.{}", ScratchFile::unguessable()?));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(contents)?;
                    return Ok(path);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => taken = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(taken.unwrap_or_else(|| io::Error::other("no free name for a scratch file")))
    }

    fn unguessable() -> io::Result<String> {
        let mut bytes = [0u8; NAME_BYTES];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes.iter().fold(String::new(), |mut name, b| {
            let _ = write!(name, "{b:02x}");
            name
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn a_scratch_file_is_new_private_and_named_apart() {
        let dir = std::env::temp_dir().join(format!("dibs-scratch-test.{}", std::process::id()));
        let a = ScratchFile::create(&dir, "t", b"x").unwrap();
        let b = ScratchFile::create(&dir, "t", b"").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(&a).unwrap(), b"x");
        let mode = fs::metadata(&a).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }
}
