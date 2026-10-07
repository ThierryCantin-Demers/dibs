//! The boundaries of what a prepare decides, pinned as text: what a copy keeps of each file,
//! which copies must share their blocks, how far ahead a sibling must be for a tree to start
//! again from it, and when a seed waits for a build.

use crate::tree::{
    copy::{Copier, Reflinks, Sharing},
    tests::{Building, Machine, lines, local, run},
};
use dibs_format::Span;
use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

/// What one entry is: a link's target, or its time as old or new, and given `modes`, its mode
/// and how many names its file has.
fn shown(path: &Path, rel: &str, modes: bool) -> String {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.file_type().is_symlink() {
        return format!("{rel} -> {}\n", fs::read_link(path).unwrap().display());
    }
    let age = SystemTime::now()
        .duration_since(meta.modified().unwrap())
        .unwrap_or_default();
    let mut line = format!("{rel}{}", if meta.is_dir() { "/" } else { "" });
    if modes {
        line += &format!("  {:o}", meta.permissions().mode() & 0o7777);
    }
    line += match age > Duration::from_secs(Span::DAY.0) {
        true => "  old",
        false => "  new",
    };
    if modes && !meta.is_dir() && meta.nlink() > 1 {
        line += &format!("  {} names", meta.nlink());
    }
    line + "\n"
}

/// Every entry under `root`, sorted by path.
fn listing(root: &Path, modes: bool) -> String {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            }
            found.push(entry.path());
        }
    }
    found.sort();
    found
        .iter()
        .map(|path| {
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            shown(path, &rel, modes)
        })
        .collect()
}

#[test]
fn a_copy_keeps_every_mode_link_and_time() {
    let m = Machine::new();
    run(
        &m.root,
        "mkdir -p from/bin from/private \
         && printf x > from/bin/tool && printf y > from/private/key \
         && ln -s bin/tool from/link && ln -s /nowhere from/dangling && ln from/bin/tool from/hard \
         && chmod 750 from/bin/tool && chmod 600 from/private/key && chmod 700 from/private \
         && chmod 755 from/bin from \
         && touch -d '400 days ago' from/bin/tool from/private/key from/private from/bin",
    );
    let (from, to) = (m.p("from"), m.p("to"));
    Copier::new(Reflinks::Copies)
        .tree(&from, &to, Sharing::Required)
        .unwrap();
    assert_eq!(listing(&to, true), listing(&from, true));
    assert_eq!(
        listing(&to, true),
        "bin/  755  old\n\
         bin/tool  750  old  2 names\n\
         dangling -> /nowhere\n\
         hard  750  old  2 names\n\
         link -> bin/tool\n\
         private/  700  old\n\
         private/key  600  old\n"
    );
}

#[test]
fn a_target_must_share_its_blocks_where_sources_may_be_copied_plainly() {
    let m = Machine::new();
    run(&m.root, "mkdir from && printf x > from/f");
    let mut said = String::new();
    for (reflinks, disk) in [
        (Reflinks::Copies, "a disk that shares blocks"),
        (Reflinks::Never, "one that cannot"),
    ] {
        for (sharing, what) in [
            (Sharing::Required, "a target"),
            (Sharing::Preferred, "sources"),
        ] {
            let to = m.p(&format!("to-{}", said.lines().count()));
            let copied = Copier::new(reflinks)
                .tree(&m.p("from"), &to, sharing)
                .is_ok();
            said += &format!(
                "{what} on {disk}: {}\n",
                if copied { "copied" } else { "refused" }
            );
        }
    }
    assert_eq!(
        said,
        "a target on a disk that shares blocks: copied\n\
         sources on a disk that shares blocks: copied\n\
         a target on one that cannot: refused\n\
         sources on one that cannot: copied\n"
    );
}

#[test]
fn a_seeded_tree_keeps_its_siblings_times_and_is_marked_used_now() {
    let m = Machine::new();
    m.sibling("demo-local-old", &["aaa"]);
    m.sources("old", "lib.rs");
    run(
        &m.root,
        "find scratch -exec touch -h -d '400 days ago' {} +",
    );
    let p = m.prepared(&local("new", &["aaa"]));
    assert_eq!(
        listing(Path::new(&p.target), false),
        ".dibs-packages  old\n\
         .dibs-packages.pending.t-new  new\n\
         .dibs-tree  new\n\
         .dibs-used  new\n\
         debug/  old\n\
         debug/.cargo-lock  old\n\
         debug/deps/  old\n\
         debug/deps/libdemo-local-old.rlib  old\n"
    );
    assert_eq!(
        listing(Path::new(&p.worktree), false),
        ".dibs-used  new\nlib.rs  old\n"
    );
}

#[test]
fn a_tree_starts_again_only_from_a_sibling_a_tenth_of_its_lockfile_ahead() {
    let all: Vec<String> = (0..20).map(|i| format!("p{i:02}")).collect();
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let mut said = String::new();
    for ahead in [6, 7] {
        let m = Machine::new();
        m.sibling("demo-local-mine", &all[..5]);
        m.sources("mine", "mine.rs");
        m.sibling("demo-local-other", &all[..ahead]);
        let again = m.prepared(&local("mine", &all)).reseeded.is_some();
        said += &format!(
            "its own target has 5 of 20, a sibling {ahead}: {}\n",
            match again {
                true => "starts again from the sibling",
                false => "keeps its own",
            }
        );
    }
    assert_eq!(
        said,
        "its own target has 5 of 20, a sibling 6: keeps its own\n\
         its own target has 5 of 20, a sibling 7: starts again from the sibling\n"
    );
}

#[test]
fn a_new_tree_waits_only_for_a_build_that_leaves_more_than_anything_idle_has() {
    let mut said = String::new();
    for (will, finishes) in [(2, true), (3, true), (3, false)] {
        let mut m = Machine::new();
        // Only a build that outlasts the wait needs it short; one that finishes ends it at once.
        m.seed_wait = Duration::from_secs(if finishes { 600 } else { 1 });
        m.sibling("demo-local-idle", &["aaa", "bbb"]);
        let busy = m.sibling("demo-local-busy", &["aaa"]);
        let after = &["aaa", "bbb", "ccc"][..will];
        fs::write(
            busy.join(".dibs-packages.pending.x"),
            lines(after).join("\n") + "\n",
        )
        .unwrap();
        let build = Mutex::new(Some(Building::holding(&busy.join("debug/.cargo-lock"))));
        let (prepared, heard) = m.prepare_hearing(&local("new", &["aaa", "bbb", "ccc"]), &|text| {
            if finishes && text.contains("waiting") {
                fs::write(busy.join(".dibs-packages"), lines(after).join("\n") + "\n").unwrap();
                build.lock().unwrap().take().unwrap().done();
            }
        });
        if let Some(build) = build.lock().unwrap().take() {
            build.done();
        }
        said += &format!(
            "idle has 2 of 3, a build will leave {will}{}: from {}{}\n",
            if finishes {
                ""
            } else {
                " but outlasts the wait"
            },
            prepared.unwrap().seeded.unwrap().from,
            if heard.contains("waiting up to ") {
                ", after waiting"
            } else {
                ""
            }
        );
    }
    assert_eq!(
        said,
        "idle has 2 of 3, a build will leave 2: from demo-local-idle\n\
         idle has 2 of 3, a build will leave 3: from demo-local-busy, after waiting\n\
         idle has 2 of 3, a build will leave 3 but outlasts the wait: from demo-local-idle, after waiting\n"
    );
}
