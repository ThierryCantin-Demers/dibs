use crate::platform::{Host, Platform as _};
use std::path::{Path, PathBuf};

/// A process and everything under it, parents first, as one look at a job reads it.
pub struct Tree {
    pids: Vec<u32>,
}

impl Tree {
    pub fn of(root: u32) -> Tree {
        Tree {
            pids: Host::tree(root),
        }
    }

    /// What runs under the root, which holds the lock for it: the root's own CPU is dibs's, and
    /// a runner's threads would make a job waiting on a pipe look busy.
    pub fn work_ticks(&self) -> u64 {
        self.pids
            .iter()
            .skip(1)
            .filter_map(|&pid| Host::cpu_ticks(pid))
            .sum()
    }

    /// The regular files the tree writes to on stdout or stderr, other than the channel its root
    /// was started on and the job log dibs itself gave it.
    pub fn written(&self) -> Vec<PathBuf> {
        let Some(&root) = self.pids.first() else {
            return Vec::new();
        };
        let channel = Host::fd_path(root, 1);
        let mut files: Vec<PathBuf> = Vec::new();
        for &pid in &self.pids {
            for fd in [1, 2] {
                let Some(path) = Host::fd_path(pid, fd).filter(|p| p.starts_with('/')) else {
                    continue;
                };
                let file = PathBuf::from(&path);
                if Some(&path) == channel.as_ref() || dibs_log(&file) || !file.is_file() {
                    continue;
                }
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        files
    }
}

/// `.../jobs/<id>/log`, which dibs pointed the job at.
fn dibs_log(file: &Path) -> bool {
    let job = file.parent();
    file.file_name().is_some_and(|n| n == "log")
        && job
            .and_then(Path::file_name)
            .is_some_and(|n| n.to_string_lossy().contains('-'))
        && job
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|n| n == "jobs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_job_log_dibs_gave_reads_as_dibs_own() {
        assert!(dibs_log(Path::new("/s/jobs/20261002-120000-42/log")));
        assert!(!dibs_log(Path::new("/s/jobs/x/log")));
        assert!(!dibs_log(Path::new("/s/build/20261002-120000-42/log")));
    }

    #[test]
    fn a_tree_starts_at_its_root_and_counts_its_time() {
        let me = std::process::id();
        let tree = Tree::of(me);
        assert_eq!(tree.pids.first(), Some(&me));
        assert!(Host::cpu_ticks(me).is_some());
    }
}
