//! What a coworker sent ahead of a submission, as the dibspatcher finds it in their stage.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Every file under `root` but its own `.git`, as paths relative to it, which is what a
/// client seals: a `.git` deeper down is a file of the tree like any other.
///
/// # Assumptions
///
/// - Every name under `root` is UTF-8, as the client's own listing of what it sends is: a name
///   that is not is listed lossily, and its file then cannot be sealed.
///
/// # Guarantees
///
/// - Paths are `/`-separated, as the client names what it seals, whatever this system writes.
pub fn files(root: &Path) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    let mut dirs = vec![(PathBuf::new(), String::new())];
    while let Some((dir, named)) = dirs.pop() {
        for entry in fs::read_dir(root.join(&dir))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if named.is_empty() && name == ".git" {
                continue;
            }
            let path = match named.as_str() {
                "" => name.clone(),
                parent => format!("{parent}/{name}"),
            };
            match entry.file_type()?.is_dir() {
                true => dirs.push((dir.join(entry.file_name()), path)),
                false => found.push(path),
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
