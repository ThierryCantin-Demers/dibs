//! `Seal::of` reads the files it seals, so each test lays out a directory of its own.

use super::Seal;
use crate::{Hex, Sha256};
use std::{fs, path::PathBuf};

/// A directory of its own for one test, holding `a` and `src/b`, removed when dropped.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Tree {
        let root = std::env::temp_dir().join(format!("dibs-seal-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("a"), "one").unwrap();
        fs::write(root.join("src/b"), "two").unwrap();
        Tree(root)
    }

    fn seal(&self, files: &[&str]) -> Seal {
        Seal::of(&self.0, files.iter().copied()).unwrap()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_seal_is_the_same_whatever_order_and_however_often_its_files_are_named() {
    let tree = Tree::new("order");
    assert_eq!(
        tree.seal(&["src/b", "a", "src/b"]),
        tree.seal(&["a", "src/b"])
    );
}

#[test]
fn a_seal_counts_each_file_once() {
    let tree = Tree::new("count");
    assert_eq!(tree.seal(&["src/b", "a", "src/b"]).file_count, 2);
}

#[test]
fn a_changed_file_changes_the_seal() {
    let tree = Tree::new("changed");
    let before = tree.seal(&["a", "src/b"]);
    fs::write(tree.0.join("src/b"), "changed").unwrap();
    assert_ne!(tree.seal(&["a", "src/b"]).digest, before.digest);
}

#[test]
fn a_seal_digests_each_files_path_and_digest_in_path_order() {
    let tree = Tree::new("format");
    let line = |path: &str, text: &str| format!("{path}\0{}\n", Sha256::hex(text.as_bytes()));
    let mut lines = Sha256::default();
    lines.update(format!("{}{}", line("a", "one"), line("src/b", "two")).as_bytes());
    assert_eq!(
        tree.seal(&["src/b", "a"]).digest,
        Hex::new(&lines.finish()).to_string()
    );
}
