use crate::{
    clock::Deadline,
    job::Environment,
    tree::{Commands, Copier, Reflinks, Stepping, Trees},
};
use dibs_format::{
    Exit,
    wire::{GitDb, Nest, Packages, Prepare, Prepared, Source},
};
use std::{
    cell::RefCell,
    fs::{self, File},
    io::{BufRead as _, BufReader},
    os::unix::{fs::FileTypeExt as _, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

/// A scratch, and a home whose `prog/demo` is a clone with a branch only it has.
struct Machine {
    root: PathBuf,
    reflinks: Reflinks,
    seed_wait: Duration,
    environment: Environment,
    deadline: Deadline,
}

impl Machine {
    fn new() -> Machine {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "dibs-trees.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("scratch")).unwrap();
        Machine {
            root,
            reflinks: Reflinks::Copies,
            seed_wait: Duration::from_secs(900),
            environment: Environment::default(),
            deadline: Deadline::default(),
        }
    }

    /// The clone, with `main` pushed, a `decoy` pushed, and `local-only` committed here alone.
    fn with_clone(self) -> Machine {
        fs::create_dir_all(self.root.join("home/prog")).unwrap();
        run(&self.root, "git init -q --bare origin.git");
        run(&self.root, "git clone -q origin.git home/prog/demo");
        let src = self.root.join("home/prog/demo");
        run(&src, "git config user.email a@b && git config user.name t");
        run(
            &src,
            "echo one > f && git add -A && git commit -qm first && git push -q origin HEAD:main",
        );
        run(
            &src,
            "git checkout -q -b decoy && echo decoy > f && git commit -qam decoy && git push -q origin decoy",
        );
        run(
            &src,
            "git checkout -q -B local-only origin/main && echo real > f && git commit -qam real",
        );
        self
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn sha(&self, reference: &str) -> String {
        run(
            &self.p("home/prog/demo"),
            &format!("git rev-parse {reference}"),
        )
    }

    fn prepare(&self, prepare: &Prepare) -> (Result<Prepared, Exit>, String) {
        self.prepare_hearing(prepare, &|_| {})
    }

    /// A prepare, with `hear` told each thing it says as it says it.
    fn prepare_hearing(
        &self,
        prepare: &Prepare,
        hear: &dyn Fn(&str),
    ) -> (Result<Prepared, Exit>, String) {
        let said = RefCell::new(String::new());
        let say = |text: &str| {
            said.borrow_mut().push_str(text);
            hear(text);
        };
        let scratch = self.p("scratch");
        let home = self.p("home");
        let cargo = self.p("home/.cargo");
        let trees = Trees {
            scratch: &scratch,
            home: &home,
            cargo_home: &cargo,
            keep_days: 14,
            target_keep_days: 5,
            seed_wait: self.seed_wait,
            copier: Copier {
                reflinks: self.reflinks,
            },
            commands: Commands {
                environment: &self.environment,
                running: &|_| {},
                deadline: self.deadline,
            },
            say: &say,
        };
        let prepared = trees.prepare(prepare);
        (prepared, said.into_inner())
    }

    fn prepared(&self, prepare: &Prepare) -> Prepared {
        let (prepared, said) = self.prepare(prepare);
        prepared.unwrap_or_else(|e| panic!("{e:?}: {said}"))
    }

    /// A sibling target with an artifact, a lock and, given lines, a record.
    fn sibling(&self, name: &str, record: &[&str]) -> PathBuf {
        let t = self.p(&format!("scratch/target/{name}"));
        fs::create_dir_all(t.join("debug/deps")).unwrap();
        fs::write(t.join(format!("debug/deps/lib{name}.rlib")), "artifact\n").unwrap();
        fs::write(t.join("debug/.cargo-lock"), "").unwrap();
        fs::write(t.join(".dibs-used"), "").unwrap();
        run(&t, "touch -d '1 hour ago' .dibs-used");
        if !record.is_empty() {
            fs::write(t.join(".dibs-packages"), lines(record).join("\n") + "\n").unwrap();
        }
        t
    }

    /// A local tree's sources.
    fn sources(&self, key: &str, file: &str) -> PathBuf {
        let ws = self.p(&format!("scratch/ws/demo/local-{key}"));
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join(file), "source\n").unwrap();
        ws
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A lock held by a process of its own: one this test held could linger in a parallel test's fork.
struct Building(Child);

impl Building {
    fn holding(lock: &Path) -> Building {
        let mut build = Command::new("flock")
            .arg(lock)
            .args(["-c", "echo held; read -r _"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut held = String::new();
        BufReader::new(build.stdout.take().unwrap())
            .read_line(&mut held)
            .unwrap();
        Building(build)
    }

    fn done(mut self) {
        drop(self.0.stdin.take());
        self.0.wait().unwrap();
    }
}

fn run(dir: &Path, cmd: &str) -> String {
    let out = Command::new("bash")
        .args(["-c", cmd])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{cmd}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn lines(of: &[&str]) -> Vec<String> {
    of.iter().map(|l| l.to_string()).collect()
}

fn aged(path: &Path) {
    run(
        Path::new("/"),
        &format!("touch -d '400 days ago' '{}'", path.display()),
    );
}

fn fetched(reference: &str, slot: u32) -> Prepare {
    Prepare {
        repo: "demo".into(),
        source: Source::Fetched {
            reference: reference.into(),
            slot,
        },
        nest: None,
        fresh: Vec::new(),
        packages: None,
        gitdbs: Vec::new(),
    }
}

fn local(key: &str, packages: &[&str]) -> Prepare {
    Prepare {
        source: Source::Local {
            key: key.into(),
            content: "c".into(),
        },
        packages: (!packages.is_empty()).then(|| Packages {
            token: format!("t-{key}"),
            lines: lines(packages),
        }),
        ..fetched("", 0)
    }
}

// A branch that exists only here cannot be fetched, and the fallback fetch writes FETCH_HEAD with
// something else entirely. Reading it resolved every such ref to one unrelated commit.
#[test]
fn a_ref_that_cannot_be_fetched_still_resolves_to_itself() {
    let m = Machine::new().with_clone();
    let p = m.prepared(&fetched("local-only", 0));
    let sha = m.sha("local-only");
    assert_eq!(p.revision.sha, sha[..12]);
    assert!(p.worktree.ends_with(&format!("ws/demo/{}", &sha[..12])));
    assert!(p.target.ends_with("target/demo"));
}

#[test]
fn two_refs_do_not_resolve_to_the_same_commit() {
    let m = Machine::new().with_clone();
    let a = m.prepared(&fetched("local-only", 0)).revision.sha;
    let b = m.prepared(&fetched("main", 0)).revision.sha;
    assert_ne!(a, b);
    assert_eq!(b, m.sha("origin/main")[..12]);
}

#[test]
fn a_ref_that_does_not_exist_is_refused_and_no_prepare_leaves_its_ref() {
    let m = Machine::new().with_clone();
    let (prepared, said) = m.prepare(&fetched("no-such-branch", 0));
    assert_eq!(prepared, Err(Exit::Setup));
    assert!(
        said.contains("dibs: no such ref in demo: no-such-branch"),
        "{said}"
    );
    m.prepared(&fetched("main", 0));
    let refs = run(&m.p("home/prog/demo"), "git for-each-ref refs/dibs");
    assert!(refs.is_empty(), "{refs}");
}

#[test]
fn a_fetch_refused_for_credentials_says_to_send_the_tree_instead() {
    let mut m = Machine::new().with_clone();
    let git = run(Path::new("/"), "type -P git");
    fs::create_dir_all(m.p("bin")).unwrap();
    fs::write(
        m.p("bin/git"),
        format!(
            "#!/bin/bash\nfor a; do [ \"$a\" = fetch ] && {{ echo 'fatal: could not read Username' >&2; exit 128; }}; done\nexec {git} \"$@\"\n"
        ),
    )
    .unwrap();
    run(&m.root, "chmod +x bin/git");
    let path = format!(
        "{}:{}",
        m.p("bin").display(),
        std::env::var("PATH").unwrap()
    );
    m.environment.set("PATH", path);
    let (prepared, said) = m.prepare(&fetched("nope", 0));
    assert_eq!(prepared, Err(Exit::Setup));
    assert!(said.contains("Send your working tree instead"), "{said}");
}

#[test]
fn a_branch_name_containing_a_slash_resolves_to_itself() {
    let m = Machine::new().with_clone();
    run(
        &m.p("home/prog/demo"),
        "git checkout -q -b feat/x origin/main && echo x > f && git commit -qam x && git push -q origin feat/x",
    );
    let p = m.prepared(&fetched("feat/x", 0));
    assert_eq!(p.revision.sha, m.sha("feat/x")[..12]);
}

#[test]
fn two_names_for_one_commit_share_its_tree() {
    let m = Machine::new().with_clone();
    run(&m.p("home/prog/demo"), "git branch same local-only");
    let a = m.prepared(&fetched("local-only", 0)).worktree;
    let b = m.prepared(&fetched("same", 0)).worktree;
    assert_eq!(a, b);
}

#[test]
fn concurrent_prepares_of_one_commit_all_succeed() {
    let m = Machine::new().with_clone();
    let all: Vec<bool> = std::thread::scope(|s| {
        let started: Vec<_> = (0..4)
            .map(|_| s.spawn(|| m.prepare(&fetched("main", 0)).0.is_ok()))
            .collect();
        started.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(all.iter().all(|ok| *ok), "{all:?}");
}

#[test]
fn a_later_arm_builds_into_a_target_of_its_own_seeded_without_a_claim() {
    let m = Machine::new().with_clone();
    m.prepared(&fetched("main", 0));
    aged(&m.p("scratch/target/demo/.dibs-used"));
    m.sibling("demo-local-k", &[]);
    m.sources("k", "theirs.rs");
    let p = m.prepared(&fetched("decoy", 1));
    assert!(p.target.ends_with("target/demo-arm1"), "{}", p.target);
    let seeded = p.seeded.unwrap();
    assert_eq!(seeded.from, "demo-local-k");
    assert!(!seeded.sources, "a fetched tree brings its own sources");
    assert!(!Path::new(&p.target).join(".dibs-tree").exists());
}

#[test]
fn a_pinned_tree_is_nested_under_its_config_and_has_a_target_of_its_own() {
    let m = Machine::new().with_clone();
    let nest = Nest {
        name: "pin-0123456789".into(),
        config: "[patch.crates-io]\nserde = { path = \"/x\" }\n".into(),
    };
    for prepare in [fetched("main", 0), local("k", &[])] {
        let p = m.prepared(&Prepare {
            nest: Some(nest.clone()),
            ..prepare
        });
        assert!(
            p.worktree.contains("ws/demo/pin-0123456789/"),
            "{}",
            p.worktree
        );
        assert!(p.target.ends_with("-pin-0123456789"), "{}", p.target);
    }
    let nested = m.p("scratch/ws/demo/pin-0123456789");
    assert_eq!(
        fs::read_to_string(nested.join(".cargo/config.toml")).unwrap(),
        nest.config
    );
    assert!(nested.join(".dibs-used").exists());
}

#[test]
fn a_local_tree_is_keyed_and_marked_and_its_lockfile_staged() {
    let m = Machine::new();
    let p = m.prepared(&local("k1", &["aaa", "bbb"]));
    assert_eq!(p.revision.sha, "local:c");
    let (wt, target) = (PathBuf::from(&p.worktree), PathBuf::from(&p.target));
    assert!(wt.ends_with("ws/demo/local-k1") && wt.join(".dibs-used").exists());
    assert!(target.ends_with("target/demo-local-k1"));
    assert_eq!(fs::read_to_string(target.join(".dibs-used")).unwrap(), "");
    assert_eq!(
        fs::read_to_string(target.join(".dibs-packages.pending.t-k1")).unwrap(),
        "aaa\nbbb\n"
    );
    assert!(m.p("scratch/out").is_dir());
}

#[test]
fn a_new_tree_starts_from_its_repos_latest_target_with_its_sources() {
    let m = Machine::new();
    let old = m.sibling("demo-local-old", &[]);
    m.sources("old", "theirs.rs");
    let p = m.prepared(&local("new", &[]));
    let seeded = p.seeded.unwrap();
    assert_eq!(
        (seeded.from.as_str(), seeded.sources),
        ("demo-local-old", true)
    );
    let target = PathBuf::from(&p.target);
    assert!(target.join("debug/deps/libdemo-local-old.rlib").exists());
    assert_eq!(
        fs::read_to_string(target.join(".dibs-tree")).unwrap(),
        format!("{}\n", p.worktree)
    );
    assert!(Path::new(&p.worktree).join("theirs.rs").exists());
    assert!(old.join("debug/deps/libdemo-local-old.rlib").exists());
}

#[test]
fn a_fifo_or_socket_in_a_siblings_sources_is_made_anew_rather_than_read() {
    let m = Machine::new();
    m.sibling("demo-local-old", &[]);
    let sources = m.sources("old", "theirs.rs");
    // Unreadable, so a copier that opened it would fail here rather than wait for a writer.
    run(&sources, "mkfifo pipe && chmod 200 pipe");
    let socket = UnixListener::bind(sources.join("socket")).unwrap();
    let p = m.prepared(&local("new", &[]));
    drop(socket);
    assert!(p.seeded.unwrap().sources);
    let kind = |name: &str| {
        fs::symlink_metadata(Path::new(&p.worktree).join(name))
            .unwrap()
            .file_type()
    };
    assert!(kind("pipe").is_fifo() && kind("socket").is_socket());
}

#[test]
fn a_seeded_tree_starts_without_exactly_what_its_repo_names() {
    let m = Machine::new();
    m.sibling("demo-local-old", &[]);
    let ws = m.sources("old", "kept.rs");
    fs::create_dir_all(ws.join("cache/x")).unwrap();
    fs::write(ws.join("cache/x/store"), "").unwrap();
    fs::write(ws.join("cachefile"), "").unwrap();
    let p = m.prepared(&Prepare {
        fresh: lines(&["cache"]),
        ..local("new", &[])
    });
    let wt = PathBuf::from(&p.worktree);
    assert!(wt.join("kept.rs").exists() && wt.join("cachefile").exists());
    assert!(!wt.join("cache").exists());
}

#[test]
fn a_target_a_build_holds_is_not_copied() {
    let m = Machine::new();
    let t = m.sibling("demo-local-old", &[]);
    let build = File::open(t.join("debug/.cargo-lock")).unwrap();
    build.lock().unwrap();
    assert_eq!(m.prepared(&local("new", &[])).seeded, None);
}

#[test]
fn a_filesystem_without_reflinks_gets_no_seed_and_no_partial_copy() {
    let mut m = Machine::new();
    m.reflinks = Reflinks::Never;
    m.sibling("demo-local-old", &[]);
    m.sources("old", "theirs.rs");
    let p = m.prepared(&local("new", &[]));
    assert_eq!(p.seeded, None);
    let left: Vec<String> = fs::read_dir(m.p("scratch/target"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".seed."))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(!Path::new(&p.worktree).join("theirs.rs").exists());
}

#[test]
fn a_tree_that_outlived_its_target_is_not_seeded() {
    let m = Machine::new();
    m.sibling("demo-local-old", &[]);
    m.sources("new", "mine.rs");
    assert_eq!(m.prepared(&local("new", &[])).seeded, None);
}

#[test]
fn the_sibling_that_built_the_most_of_this_lockfile_wins_over_a_newer_one() {
    let m = Machine::new();
    let old = m.sibling("demo-local-old", &["aaa", "bbb"]);
    aged(&old.join(".dibs-used"));
    m.sibling("demo-local-newer", &["aaa", "ccc"]);
    let seeded = m.prepared(&local("new", &["aaa", "bbb"])).seeded.unwrap();
    assert_eq!(seeded.from, "demo-local-old");
    assert_eq!(seeded.shared.map(|s| (s.have, s.of)), Some((2, 2)));
}

#[test]
fn among_equal_matches_the_newest_sibling_wins() {
    let m = Machine::new();
    let old = m.sibling("demo-local-old", &[]);
    aged(&old.join(".dibs-used"));
    // Named to sort after the older sibling, so only recency can put it first.
    m.sibling("demo-local-zz-newer", &[]);
    let seeded = m.prepared(&local("new", &["aaa"])).seeded.unwrap();
    assert_eq!(seeded.from, "demo-local-zz-newer");
}

#[test]
fn an_existing_tree_far_behind_a_sibling_starts_again_from_it() {
    let m = Machine::new();
    m.sibling("demo-local-moved", &["aaa", "bbb"]);
    m.sources("moved", "theirs.rs");
    m.sibling("demo-local-mine", &["aaa", "ccc"]);
    m.sources("mine", "mine.rs");
    let p = m.prepared(&local("mine", &["aaa", "bbb"]));
    assert_eq!(p.reseeded, Some(1));
    let seeded = p.seeded.unwrap();
    assert_eq!(seeded.from, "demo-local-moved");
    assert_eq!(seeded.shared.map(|s| (s.have, s.of)), Some((2, 2)));
    let (t, ws) = (PathBuf::from(&p.target), PathBuf::from(&p.worktree));
    assert!(t.join("debug/deps/libdemo-local-moved.rlib").exists());
    assert!(!t.join("debug/deps/libdemo-local-mine.rlib").exists());
    assert!(ws.join("theirs.rs").exists() && !ws.join("mine.rs").exists());
    let left: Vec<_> = fs::read_dir(m.p("scratch/target"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".old."))
        .collect();
    assert!(left.is_empty(), "nothing of the old tree is left behind");
}

#[test]
fn a_tree_prepared_moments_ago_or_worked_in_is_never_moved_aside() {
    let m = Machine::new();
    m.sibling("demo-local-moved", &["aaa", "bbb"]);
    m.sources("moved", "theirs.rs");
    let mine = m.sibling("demo-local-mine", &["aaa", "ccc"]);
    let ws = m.sources("mine", "mine.rs");
    fs::write(mine.join(".dibs-used"), "").unwrap();
    let p = m.prepared(&local("mine", &["aaa", "bbb"]));
    assert_eq!(
        p.reseeded, None,
        "another call's job may be about to enter a tree prepared a moment ago"
    );
    aged(&mine.join(".dibs-used"));
    let mut worker = Command::new("sh")
        .args(["-c", "read -r _"])
        .current_dir(&ws)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let p = m.prepared(&local("mine", &["aaa", "bbb"]));
    let _ = worker.kill();
    let _ = worker.wait();
    assert_eq!(p.reseeded, None, "and a process working in it is using it");
    assert!(ws.join("mine.rs").exists(), "so it keeps its own");
}

#[test]
fn a_reseed_that_cannot_remove_what_it_replaced_still_prepares_the_tree() {
    let m = Machine::new();
    m.sibling("demo-local-moved", &["aaa", "bbb"]);
    let mine = m.sibling("demo-local-mine", &["aaa", "ccc"]);
    m.sources("mine", "mine.rs");
    let stuck = mine.join("stuck");
    fs::create_dir_all(&stuck).unwrap();
    fs::write(stuck.join("f"), "").unwrap();
    run(&stuck, "chmod 500 .");
    let (prepared, said) = m.prepare(&local("mine", &["aaa", "bbb"]));
    run(&m.p("scratch"), "chmod -R u+w target");
    assert_eq!(prepared.unwrap().reseeded, Some(1), "{said}");
    assert!(
        said.contains("could not remove all of what the reseed replaced; the next sweep takes it"),
        "{said}"
    );
}

/// What a reseed and the call beside it were heard to do.
#[derive(Debug, PartialEq, Eq)]
enum Heard {
    Copying,
    Queued,
    Synced,
}

/// A reseed of `mine` that waits for the build in `busy`, which will have all of its lockfile.
fn reseed_waiting_on_a_build(m: &Machine) -> (Building, PathBuf) {
    let busy = m.sibling("demo-local-busy", &["aaa"]);
    fs::write(busy.join(".dibs-packages.pending.x"), "aaa\nbbb\n").unwrap();
    m.sibling("demo-local-mine", &["ccc"]);
    (Building::holding(&busy.join("debug/.cargo-lock")), busy)
}

#[test]
fn a_reseed_never_replaces_a_tree_another_prepare_handed_over_while_it_copied() {
    let m = &Machine::new();
    let ws = &m.sources("mine", "mine.rs");
    let (build, busy) = reseed_waiting_on_a_build(m);
    let (tell, heard) = mpsc::channel();
    let (reseeded, first) = thread::scope(|s| {
        let reseeding = tell.clone();
        let reseed = s.spawn(move || {
            m.prepare_hearing(&local("mine", &["aaa", "bbb"]), &|text| {
                if text.contains("waiting up to") {
                    reseeding.send(Heard::Copying).unwrap();
                }
            })
        });
        assert_eq!(heard.recv().unwrap(), Heard::Copying);
        let sync = s.spawn(move || {
            let (prepared, said) = m.prepare_hearing(&local("mine", &[]), &|text| {
                if text.contains("another prepare") {
                    tell.send(Heard::Queued).unwrap();
                }
            });
            prepared.unwrap_or_else(|e| panic!("{e:?}: {said}"));
            fs::write(ws.join("synced.rs"), "sent\n").unwrap();
            tell.send(Heard::Synced).unwrap();
        });
        let first = heard.recv().unwrap();
        fs::write(busy.join(".dibs-packages"), "aaa\nbbb\n").unwrap();
        build.done();
        sync.join().unwrap();
        (reseed.join().unwrap(), first)
    });
    assert_eq!(first, Heard::Queued, "the second prepare waits its turn");
    let (reseeded, said) = reseeded;
    assert_eq!(reseeded.unwrap().reseeded, Some(0), "{said}");
    assert!(
        ws.join("synced.rs").exists(),
        "what was sent once the reseed had decided is still there"
    );
}

#[test]
fn a_reseed_keeps_a_tree_a_process_entered_while_it_copied() {
    let m = Machine::new();
    let ws = m.sources("mine", "mine.rs");
    let (build, busy) = reseed_waiting_on_a_build(&m);
    let build = Mutex::new(Some(build));
    let worker = Mutex::new(None);
    let (prepared, said) = m.prepare_hearing(&local("mine", &["aaa", "bbb"]), &|text| {
        if text.contains("waiting up to") {
            let entered = Command::new("sh")
                .args(["-c", "read -r _"])
                .current_dir(&ws)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            *worker.lock().unwrap() = Some(entered);
            fs::write(busy.join(".dibs-packages"), "aaa\nbbb\n").unwrap();
            if let Some(build) = build.lock().unwrap().take() {
                build.done();
            }
        }
    });
    if let Some(mut entered) = worker.lock().unwrap().take() {
        drop(entered.stdin.take());
        entered.wait().unwrap();
    }
    assert_eq!(prepared.unwrap().reseeded, None, "{said}");
    assert!(ws.join("mine.rs").exists(), "it keeps its own");
    let left: Vec<_> = fs::read_dir(m.p("scratch/target"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".seed."))
        .collect();
    assert!(left.is_empty(), "and the copy goes");
}

#[test]
fn an_existing_tree_close_to_its_siblings_or_being_built_keeps_its_own() {
    let m = Machine::new();
    m.sibling("demo-local-moved", &["aaa", "bbb"]);
    m.sibling("demo-local-mine", &["aaa", "bbb"]);
    m.sources("mine", "mine.rs");
    let p = m.prepared(&local("mine", &["aaa", "bbb"]));
    assert_eq!((p.reseeded, p.seeded), (None, None));
    let behind = m.sibling("demo-local-behind", &["ccc"]);
    m.sources("behind", "behind.rs");
    let build = File::open(behind.join("debug/.cargo-lock")).unwrap();
    build.lock().unwrap();
    let p = m.prepared(&local("behind", &["aaa", "bbb"]));
    assert_eq!(p.reseeded, None, "a build holds it");
    assert!(m.p("scratch/ws/demo/local-behind/behind.rs").exists());
}

#[test]
fn a_reseed_never_waits_on_its_own_target_set_aside() {
    let mut m = Machine::new();
    m.seed_wait = Duration::from_secs(3);
    let mine = m.sibling("demo-local-mine", &["ccc"]);
    fs::write(mine.join(".dibs-packages.pending.failed"), "aaa\n").unwrap();
    m.sources("mine", "mine.rs");
    let (prepared, said) = m.prepare(&local("mine", &["aaa"]));
    assert!(!said.contains("waiting"), "{said}");
    assert_eq!(prepared.unwrap().reseeded, None);
    assert!(mine.join("debug/deps/libdemo-local-mine.rlib").exists());
}

#[test]
fn a_new_tree_waits_for_a_sibling_building_more_of_its_lockfile() {
    let mut m = Machine::new();
    m.seed_wait = Duration::from_secs(30);
    m.sibling("demo-local-idle", &["aaa"]);
    let busy = m.sibling("demo-local-busy", &["aaa"]);
    fs::write(busy.join(".dibs-packages.pending.x"), "bbb\n").unwrap();
    let build = File::open(busy.join("debug/.cargo-lock")).unwrap();
    build.lock().unwrap();
    let build = Mutex::new(Some(build));
    let (prepared, said) = m.prepare_hearing(&local("new", &["aaa", "bbb"]), &|text| {
        if text.contains("waiting") {
            fs::write(busy.join(".dibs-packages"), "aaa\nbbb\n").unwrap();
            build.lock().unwrap().take();
        }
    });
    assert!(
        said.contains("waiting up to 30s for the build in demo-local-busy"),
        "{said}"
    );
    assert_eq!(prepared.unwrap().seeded.unwrap().from, "demo-local-busy");
}

#[test]
fn a_sweep_collects_old_trees_targets_and_jobs_of_every_repo() {
    let m = Machine::new();
    let s = |rel: &str| m.p(&format!("scratch/{rel}"));
    for dir in ["ws/other/old", "ws/other/unmarked", "jobs/old", "jobs/new"] {
        fs::create_dir_all(s(dir)).unwrap();
    }
    fs::write(s("ws/other/old/.dibs-used"), "").unwrap();
    aged(&s("ws/other/old/.dibs-used"));
    aged(&s("jobs/old"));
    let old = m.sibling("other", &[]);
    aged(&old.join(".dibs-used"));
    let unmarked = s("target/unmarked");
    fs::create_dir_all(unmarked.join("release")).unwrap();
    let swept = s("target/swept-only");
    fs::create_dir_all(&swept).unwrap();
    fs::write(swept.join(".dibs-used"), "swept\n").unwrap();
    let p = m.prepared(&local("k", &[]));
    assert!(!s("ws/other/old").exists() && !s("jobs/old").exists() && !old.exists());
    assert!(
        s("ws/other/unmarked/.dibs-used").exists(),
        "an unmarked tree is dated"
    );
    assert_eq!(
        fs::read_to_string(unmarked.join(".dibs-used")).unwrap(),
        "swept\n",
        "an unmarked target is dated"
    );
    assert!(
        !swept.exists(),
        "a target holding only a sweep's marker is no cache"
    );
    assert!(s("jobs/new").exists() && Path::new(&p.worktree).exists());
}

#[test]
fn a_sweep_leaves_a_target_a_build_holds_and_says_what_it_could_not_remove() {
    let m = Machine::new();
    let held = m.sibling("held", &[]);
    aged(&held.join(".dibs-used"));
    let build = File::open(held.join("debug/.cargo-lock")).unwrap();
    build.lock_shared().unwrap();
    let stuck = m.sibling("stuck", &[]);
    aged(&stuck.join(".dibs-used"));
    run(&stuck, "chmod 500 debug/deps");
    let (prepared, said) = m.prepare(&local("k", &[]));
    run(&stuck, "chmod 700 debug/deps");
    assert!(prepared.is_ok());
    assert!(held.exists(), "a target a build holds is not swept");
    assert!(said.contains("could not remove all of"), "{said}");
    assert!(stuck.join("debug/deps").exists() && !stuck.join(".dibs-used").exists());
}

#[test]
fn a_sweep_never_removes_the_tree_just_prepared() {
    let m = Machine::new();
    let ws = m.sources("k", "f.rs");
    fs::write(ws.join(".dibs-used"), "").unwrap();
    aged(&ws.join(".dibs-used"));
    let t = m.sibling("demo-local-k", &[]);
    aged(&t.join(".dibs-used"));
    let p = m.prepared(&local("k", &[]));
    assert!(Path::new(&p.worktree).join("f.rs").exists() && Path::new(&p.target).exists());
}

#[test]
fn the_machine_says_which_git_databases_it_lacks() {
    let m = Machine::new().with_clone();
    let commit = m.sha("origin/main");
    run(
        &m.root,
        "git clone -q --bare origin.git home/.cargo/git/db/demo-0123456789abcdef",
    );
    let asked = |name: &str| GitDb {
        name: name.into(),
        commit: commit.clone(),
    };
    let p = m.prepared(&Prepare {
        gitdbs: vec![
            asked("demo-0123456789abcdef"),
            asked("gone-0123456789abcdef"),
        ],
        ..local("k", &[])
    });
    let gitdbs = p.gitdbs.unwrap();
    assert!(gitdbs.dir.ends_with("home/.cargo/git/db"));
    assert_eq!(gitdbs.missing, vec![asked("gone-0123456789abcdef")]);
    assert_eq!(m.prepared(&local("k", &[])).gitdbs, None);
}

/// A tree whose source is a year old, its target claimed by `claimed_by`, and what was said.
struct Stepped {
    machine: Machine,
    worktree: PathBuf,
    target: PathBuf,
    said: RefCell<String>,
}

impl Stepped {
    fn new(claimed_by: Option<&str>) -> Stepped {
        let machine = Machine::new();
        let worktree = machine.sources("k", "lib.rs");
        aged(&worktree.join("lib.rs"));
        let target = machine.p("scratch/target/demo-local-k");
        fs::create_dir_all(&target).unwrap();
        let mine = worktree.display().to_string();
        if let Some(by) = claimed_by {
            let by = if by == "this" { mine.as_str() } else { by };
            fs::write(target.join(".dibs-tree"), format!("{by}\n")).unwrap();
        }
        Stepped {
            machine,
            worktree,
            target,
            said: RefCell::new(String::new()),
        }
    }

    fn with<T>(&self, job_dir: Option<&Path>, act: impl FnOnce(&Stepping) -> T) -> T {
        let say = |text: &str| self.said.borrow_mut().push_str(text);
        act(&Stepping {
            worktree: &self.worktree,
            target: &self.target,
            job_dir,
            say: &say,
        })
    }

    fn lib_age(&self) -> u64 {
        fs::metadata(self.worktree.join("lib.rs"))
            .unwrap()
            .modified()
            .unwrap()
            .elapsed()
            .unwrap_or_default()
            .as_secs()
    }
}

#[test]
fn a_build_after_another_tree_s_dates_this_tree_s_sources_after_it() {
    let s = Stepped::new(Some("/another/tree"));
    s.with(None, |step| step.claim());
    assert!(s.said.borrow().contains("did not make the last build"));
    assert!(s.lib_age() < 3600);
    assert_eq!(
        fs::read_to_string(s.target.join(".dibs-tree")).unwrap(),
        format!("{}\n", s.worktree.display())
    );
}

// A rerun of one tree compiles nothing and is right to, so nothing is redated for it.
#[test]
fn a_build_after_the_same_tree_s_leaves_its_sources_alone() {
    let s = Stepped::new(Some("this"));
    s.with(None, |step| step.claim());
    assert!(s.said.borrow().is_empty());
    assert!(s.lib_age() > 86400 * 300);
}

#[test]
fn a_measurement_runs_only_where_its_own_tree_made_the_last_build() {
    for (claimed_by, refused) in [
        (Some("this"), false),
        (Some("/another/tree"), true),
        (None, true),
    ] {
        let s = Stepped::new(claimed_by);
        assert_eq!(
            s.with(None, |step| step.refused()),
            refused,
            "{claimed_by:?}"
        );
        assert_eq!(
            s.said.borrow().contains("refused to measure"),
            refused,
            "{claimed_by:?}"
        );
    }
}

#[test]
fn a_build_records_only_what_its_own_prepare_staged_and_keeps_what_was_there() {
    let s = Stepped::new(None);
    fs::write(s.target.join(".dibs-packages"), "old\n").unwrap();
    fs::write(s.target.join(".dibs-packages.pending.t1"), "aaa\nbbb\n").unwrap();
    fs::write(s.target.join(".dibs-packages.pending.t2"), "ccc\n").unwrap();
    s.with(None, |step| step.record("t1"));
    assert_eq!(
        fs::read_to_string(s.target.join(".dibs-packages")).unwrap(),
        "aaa\nbbb\nold\n"
    );
    assert!(!s.target.join(".dibs-packages.pending.t1").exists());
    assert!(s.target.join(".dibs-packages.pending.t2").exists());
    s.with(None, |step| step.record("gone"));
    assert_eq!(
        fs::read_to_string(s.target.join(".dibs-packages")).unwrap(),
        "aaa\nbbb\nold\n",
        "a build with nothing staged records nothing"
    );
}

#[test]
fn a_step_keeps_the_files_it_wrote_and_none_an_earlier_run_left() {
    let s = Stepped::new(None);
    let job = s.machine.p("scratch/jobs/j1");
    fs::create_dir_all(&job).unwrap();
    fs::create_dir_all(s.worktree.join("results")).unwrap();
    fs::write(s.worktree.join("results/old.json"), "old").unwrap();
    aged(&s.worktree.join("results/old.json"));
    fs::write(job.join("cmd"), "x").unwrap();
    aged(&job.join("cmd"));
    run(&job, "touch -d '1 minute ago' cmd");
    fs::write(s.worktree.join("results/new.json"), "new").unwrap();
    fs::create_dir_all(s.target.join("criterion/gemm")).unwrap();
    fs::write(s.target.join("criterion/gemm/estimates.json"), "e").unwrap();
    let kept = s.with(Some(&job), |step| {
        step.keep(&lines(&[
            "results/*.json",
            "$CARGO_TARGET_DIR/criterion/**/estimates.json",
        ]))
    });
    assert_eq!(kept, Some(2));
    assert!(job.join("artifacts/results/new.json").exists());
    assert!(
        job.join("artifacts/target/criterion/gemm/estimates.json")
            .exists()
    );
    assert!(!job.join("artifacts/results/old.json").exists());
    assert_eq!(s.with(Some(&job), |step| step.keep(&[])), None);
}

#[test]
fn a_file_a_claim_dated_is_not_kept_as_the_steps_own() {
    let s = Stepped::new(Some("/another/tree"));
    let job = s.machine.p("scratch/jobs/j1");
    fs::create_dir_all(&job).unwrap();
    fs::create_dir_all(s.worktree.join("results")).unwrap();
    fs::write(s.worktree.join("results/old.json"), "old").unwrap();
    aged(&s.worktree.join("results/old.json"));
    fs::write(job.join("cmd"), "x").unwrap();
    run(&job, "touch -d '1 minute ago' cmd");
    let kept = s.with(Some(&job), |step| {
        step.claim();
        fs::write(s.worktree.join("results/new.json"), "new").unwrap();
        step.keep(&lines(&["results/*.json"]))
    });
    assert_eq!(kept, Some(1));
    assert!(!job.join("artifacts/results/old.json").exists());
}

#[test]
fn a_build_that_still_takes_a_pinned_crate_from_git_is_said() {
    let s = Stepped::new(None);
    fs::write(
        s.worktree.join("Cargo.lock"),
        "[[package]]\nname = \"cubecl\"\nversion = \"0.1.0\"\nsource = \"git+https://example.invalid/cubecl?rev=a#a\"\n\n[[package]]\nname = \"cubek\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    assert!(!s.with(None, |step| step.unpinned(&lines(&["cubek"]))));
    assert!(s.with(None, |step| step.unpinned(&lines(&["cubecl"]))));
    assert!(
        s.said
            .borrow()
            .contains("  cubecl from git+https://example.invalid/cubecl?rev=a#a\n")
    );
}

#[test]
fn a_sweep_collects_replaced_runners_and_dead_builds_but_never_beside_a_build() {
    let m = Machine::new();
    let runners = m.p("home/.cache/dibs/runner");
    for hash in ["0000000000000000", "1111111111111111", "2222222222222222"] {
        fs::create_dir_all(runners.join(hash)).unwrap();
        fs::write(runners.join(hash).join("dibs-runner"), "").unwrap();
    }
    aged(&runners.join("0000000000000000/dibs-runner"));
    aged(&runners.join("1111111111111111/dibs-runner"));
    fs::create_dir_all(runners.join(".src.2222222222222222.7")).unwrap();
    let build = Building::holding(&runners.join(".build.lock"));
    m.prepared(&local("k", &[]));
    assert!(
        runners.join(".src.2222222222222222.7").exists(),
        "nothing goes while a build runs"
    );
    build.done();
    m.prepared(&local("k", &[]));
    assert!(!runners.join("0000000000000000").exists());
    assert!(!runners.join("1111111111111111").exists());
    assert!(
        runners.join("2222222222222222").exists(),
        "the newest stays"
    );
    assert!(!runners.join(".src.2222222222222222.7").exists());
}
