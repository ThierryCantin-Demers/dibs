//! `files` reads a directory, so each test lays out one of its own.

use super::files;
use std::{fs, path::PathBuf};

/// A stage of its own for one test, removed when dropped.
struct Stage(PathBuf);

impl Stage {
    fn new(name: &str) -> Stage {
        let root =
            std::env::temp_dir().join(format!("dibspatcher-stage-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/.git")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a"), "one").unwrap();
        fs::write(root.join("src/b"), "two").unwrap();
        fs::write(root.join(".git/HEAD"), "not sealed").unwrap();
        fs::write(root.join("src/.git/x"), "a nested one is").unwrap();
        Stage(root)
    }

    fn files(&self) -> Vec<String> {
        let mut found = files(&self.0).unwrap();
        found.sort();
        found
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_stage_lists_every_file_but_its_own_git_directory_by_slash_separated_paths() {
    assert_eq!(Stage::new("listed").files(), ["a", "src/.git/x", "src/b"]);
}
