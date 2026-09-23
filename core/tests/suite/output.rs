use crate::harness::*;
use std::fs;

#[test]
fn out_reads_the_file_a_running_job_redirected_into() {
    // A redirect is an open descriptor the kernel will name, and the one an agent writes is below
    // the shells the wrapper puts in the way, hence a walk rather than a look at the holder alone.
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let cmd = format!(
        "bash -c '{{ echo first line; echo second line; {}; {}; }} > {} 2>&1'",
        up.signal(),
        release.hold(),
        s.p("job.log")
    );
    let job = s.spawn(s.dibs(["--label", "writes-a-log", &cmd]));
    up.reached();
    let out = s.dibs(["--out"]).run().stdout;
    assert_eq!(out.lines_with("job.log"), 1, "it finds the file a nested redirect opened");
    assert_eq!(out.lines_with("second line"), 1, "and shows what is in it");
    let pid = s.records("holder")[0][1].clone();
    assert_eq!(s.dibs(["--out", &pid]).run().stdout.lines_with("second line"), 1, "naming the job's own pid works too");
    release.open();
    s.wait(job);
}

#[test]
fn out_reads_a_job_that_redirected_nowhere_from_its_own_sink() {
    // The wrapper's stdout is inherited by everything in the tree, and reporting it would show
    // the caller's terminal instead of the job's own output.
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let job = s.spawn(
        s.dibs(["--label", "no-log", &format!("{}; {}", up.signal(), release.hold())]).stdout_to(&s.path("caller.txt")),
    );
    up.reached();
    let out = s.dibs(["--out"]).run().stdout;
    assert_eq!(out.lines_matching("jobs/.*/log"), 1, "a job that does not redirect is read from its own sink");
    assert_eq!(out.lines_with("caller.txt"), 0, "and does not offer the caller's own stdout as output");
    release.open();
    s.wait(job);
    assert_eq!(s.dibs(["--out"]).run().stdout.lines_with("Nothing is running"), 1, "with nothing running it says so");
    assert_eq!(
        s.dibs(["--out", "999999"]).run().stderr.lines_with("Nothing holding"),
        1,
        "an unknown pid is an error, not an empty answer"
    );
}

#[test]
fn a_job_ends_with_a_trailer_and_keeps_its_whole_log() {
    // Everything a job prints is kept whole on the machine, and the caller ends with a trailer no
    // pipe on its side can cut off: the exit, who produced it, and where the log is.
    let s = Sandbox::new();
    let out = s.dibs(["--label", "j1", "echo hello; echo err >&2; exit 4"]).run();
    assert_eq!(out.code, 4, "the exit status is the command's");
    assert_eq!(out.stdout, "hello\nerr\n", "both streams reach the caller, in order");
    assert_eq!(
        out.stderr.lines_matching(r"^job [0-9]*-[0-9]*  shared  j1  queued [0-9]*s  ran [0-9]*s  exit 4  by=command$"),
        1,
        "the trailer names the job and the exit"
    );
    assert_eq!(out.stderr.lines_with("Prefer a recipe"), 0, "the recipe nag is gone");
    assert_eq!(out.stderr.lines_with("built="), 0, "a job that is not cargo says nothing about it");
    let job = job_id(&out.stderr);
    assert_eq!(s.read(&format!("scratch/jobs/{job}/log")), "hello\nerr\n", "the log holds everything the job printed");
    assert_eq!(s.read(&format!("scratch/jobs/{job}/cmd")).trim_end(), "echo hello; echo err >&2; exit 4", "and the command, whole");
    let shown = s.dibs(["--out", &job]).run().all();
    assert_eq!(shown.lines_with("| err"), 1, "--out reads a finished job by its id");
    assert_eq!(shown.lines_matching("ran [0-9]*s  exit 4"), 1, "and says how it ended");
    assert_eq!(s.dibs(["--out", "19700101-1"]).code(), 1, "an unknown job is refused");
}

#[test]
fn a_long_output_is_a_digest_and_stream_is_the_whole() {
    // A caller cannot know whether a job prints 3 lines or 30000, so the tool bounds it and says
    // what it left out and where the rest is.
    let s = Sandbox::new();
    let out = s.dibs(["--label", "j2", "seq 1 300"]).run();
    assert_eq!(out.stdout.lines().count(), 43, "a long output is a digest");
    assert_eq!(out.stdout.lines_with("260 lines omitted"), 1, "that says what it left out");
    assert_eq!(out.stdout.lines().last(), Some("300"), "and ends with the end");
    let log = s.read(&format!("scratch/jobs/{}/log", job_id(&out.stderr)));
    assert_eq!(log.lines().count(), 300, "and the log is whole either way");
    assert_eq!(s.dibs(["--stream", "--label", "j3", "seq 1 300"]).run().stdout.lines().count(), 300, "--stream is the whole output");
}

#[test]
fn the_trailer_counts_what_cargo_compiled() {
    // "Finished" with nothing compiled is the sentence that invalidates the numbers after it.
    let s = Sandbox::new();
    let built = s.dibs(["--label", "j4", "echo cargo; echo '   Compiling a v1'; echo '   Compiling b v1'; echo '    Finished release'"]).run();
    assert_eq!(built.stderr.lines_with("built=2"), 1, "the trailer counts what cargo compiled");
    let nothing = s.dibs(["--label", "j5", "echo cargo; echo '    Finished release'"]).run();
    assert_eq!(nothing.stderr.lines_with("built=nothing"), 1, "and says when it compiled nothing");
    assert_eq!(nothing.stderr.lines_with("integer expected"), 0, "and counting nothing is not an error");
    assert_eq!(nothing.stderr.lines_with("measures the previous binary"), 1, "in words");
}

#[test]
fn a_failure_run_again_unchanged_says_it_already_failed() {
    // A failing command re-run unchanged is the most repeated line in the log.
    let s = Sandbox::new();
    let first = s.dibs(["--label", "j6", "echo same; exit 9"]).run();
    let second = s.dibs(["--label", "j6", "echo same; exit 9"]).run();
    assert_eq!(first.stderr.lines_with("already failed"), 0, "the first failure says nothing about repeats");
    assert_eq!(
        second.stderr.lines_matching("already failed here: job [0-9-]* exit 9"),
        1,
        "the second says it is the same failure"
    );
}

#[test]
fn a_finished_log_once_read_is_kept_here() {
    // A log someone read stays readable here once its machine is asleep, gone or past two weeks.
    let s = Sandbox::new();
    let job = job_id(&s.dibs(["--label", "keepme", "seq 1 50"]).run().stderr);
    let out = s.dibs(["out", &job]).run().stdout;
    let (there, here) = (s.read(&format!("scratch/jobs/{job}/log")), s.read(&format!("home/.local/state/dibs/jobs/{job}/log")));
    assert!(!here.is_empty() && there == here, "reading a finished job's log keeps the whole of it here");
    assert_eq!(
        out.lines_matching(&format!("^  kept on this computer: .*/dibs/jobs/{job}/log")),
        1,
        "and says where the copy is"
    );
    assert_eq!(out.lines().last(), Some("  | 50"), "and shows its end");
    fs::remove_dir_all(s.path(&format!("scratch/jobs/{job}"))).unwrap();
    assert_eq!(
        s.dibs(["out", &job]).run().stdout.lines_matching(&format!("^job {job}  shared  keepme  ran [0-9]*s  exit 0$")),
        1,
        "which answers once the machine's copy is gone"
    );
}

#[test]
fn a_running_jobs_log_is_shown_but_not_kept() {
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let job = s.spawn(s.dibs(["--label", "keeprun", &format!("{}; {}", up.signal(), release.hold())]));
    up.reached();
    let id = fs::read_dir(s.path("scratch/jobs"))
        .unwrap()
        .flatten()
        .find(|e| fs::read_to_string(e.path().join("cmd")).unwrap_or_default().contains("f-up"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .unwrap();
    assert_eq!(s.dibs(["out", &id]).run().stdout.lines_with("still running"), 1, "a running job's log is shown");
    assert!(!s.exists(&format!("home/.local/state/dibs/jobs/{id}")), "but not kept, since it is not the whole of it");
    release.open();
    s.wait(job);
}

#[test]
fn an_overrun_says_it_was_stopped_and_what_running_it_again_does() {
    // A long compile hitting --max is ordinary, and without the message exit 124 reads as the
    // job having gone wrong.
    let s = Sandbox::new();
    let out = s.dibs(["--max", "1", "--label", "overran", "python3 -c 'while True: pass'"]).run();
    assert_eq!(out.code, 124, "an overrun exits 124");
    assert_eq!(out.all().lines_with("stopped after holding"), 1, "it says it was stopped, not that it failed");
    assert_eq!(out.all().lines_with("picks up from the crates"), 1, "and what running it again would do");
    assert_eq!(
        s.dibs(["--label", "overran", "true"]).run().stderr.lines_with("may hold"),
        0,
        "a label that fits the default hears nothing"
    );
}

#[test]
fn a_label_whose_history_runs_long_gets_a_cap_from_it() {
    // A suite that always runs past the default cap was killed as an overrun every time.
    let s = Sandbox::new();
    s.history("shared\tlong-suite\t1500\tx\nshared\tlong-suite\t1500\tx\nshared\tlong-suite\t1500\tx\n");
    assert_eq!(
        s.dibs(["--label", "long-suite", "true"]).run().stderr.lines_with("may hold the lock for 50m00s rather than 30m00s"),
        1,
        "a label whose history runs long gets a cap from it, said when it starts"
    );
    assert_eq!(s.dibs(["--max", "60", "--label", "long-suite", "true"]).run().stderr.lines_with("may hold"), 0, "unless the caller chose one");
}

#[test]
fn the_cap_comes_from_the_same_procedure() {
    // One recipe on two backends is one label and two costs, and a cap taken from the cheap one
    // kills the dear one at 124. The recipe layer names the procedure it is about to run.
    let s = Sandbox::new();
    let run = |fp: &str, label: &str| s.dibs(["--label", label, "true"]).env("DIBS_FINGERPRINT", fp).run().stderr;
    run("aaaa1111", "two-shapes");
    let last = s.read("history").lines().rfind(|l| l.split('\t').nth(1) == Some("two-shapes")).unwrap().to_string();
    assert_eq!(last.split('\t').nth(4), Some("aaaa1111"), "a run files its duration under the procedure as well as the label");
    s.history(&"shared\ttwo-shapes\t1500\tx\taaaa1111\n".repeat(3));
    s.history(&"shared\ttwo-shapes\t2\tx\tbbbb2222\n".repeat(3));
    s.history(&"shared\tlong-suite\t1500\tx\n".repeat(3));
    assert_eq!(run("aaaa1111", "two-shapes").lines_with("may hold the lock for 50m00s"), 1, "the cap comes from the same procedure");
    assert_eq!(run("bbbb2222", "two-shapes").lines_with("may hold"), 0, "and the cheap one is not given the dear one's cap");
    // A procedure that has never run is estimated from the label exactly as before.
    assert_eq!(
        run("cccc3333", "long-suite").lines_with("may hold the lock for 50m00s"),
        1,
        "a procedure with no history of its own falls back to the label"
    );
}
