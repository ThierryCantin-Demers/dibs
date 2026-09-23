use crate::harness::*;
use std::net::TcpListener;

/// A service that says its pid, which exec keeps, and then blocks until it is stopped.
fn service(s: &Sandbox, name: &str, first: &str) -> String {
    let never = s.gate(&format!("{name}-never"));
    format!("echo $$ > {}; {first}exec bash -c 'read -r _ <> {}'", s.p(&format!("{name}.pid")), never.path.display())
}

fn pid_file(s: &Sandbox, name: &str) -> u32 {
    s.read(&format!("{name}.pid")).trim().parse().unwrap()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn a_service_answering_on_its_port_is_ready_before_the_command_runs() {
    let s = Sandbox::new();
    let port = free_port();
    let listen = format!("python3 -c 'import signal, socket; s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind((\"127.0.0.1\", {port})); s.listen(); signal.pause()'");
    let connect = format!("python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", {port})); print(\"answered\")'");
    let out = s.dibs(["--label", "with-tcp", "--with", &format!("srv={listen}"), "--ready", &format!("tcp:{port}"), &connect]).run();
    assert_eq!(out.stdout, "answered\n");
}

#[test]
fn a_service_is_stopped_when_the_command_ends_whatever_its_exit() {
    let s = Sandbox::new();
    let out = s.dibs(["--label", "with-exit", "--with", &format!("srv={}", service(&s, "w1", "")), "exit 3"]).run();
    assert_eq!(out.code, 3, "the call exits with the command's status");
    assert!(!alive(pid_file(&s, "w1")), "and the service is stopped when the command ends, whatever its exit");
    assert_eq!(
        out.all().lines_matching("^  with srv: ready after [0-9]*s, stopped when the command ended  log "),
        1,
        "the trailer says how the service went"
    );
}

#[test]
fn readiness_can_be_a_command() {
    let s = Sandbox::new();
    let up = s.p("w2.up");
    let srv = service(&s, "w2", &format!("echo up > {up}; "));
    let out = s.dibs(["--label", "with-cmd", "--with", &format!("srv={srv}"), "--ready", &format!("test -s {up}"), &format!("cat {up}")]).run();
    assert_eq!(out.stdout, "up\n");
}

#[test]
fn a_service_that_exits_before_it_is_ready_ends_the_call_with_77() {
    let s = Sandbox::new();
    let out = s.dibs(["--label", "with-dies", "--with", "bad=echo oops; exit 4", "--ready", "false", &format!("touch {}", s.p("w-ran"))]).run();
    assert_eq!(out.code, 77, "a service that exits before it is ready ends the call with 77");
    assert!(!s.exists("w-ran"), "and the command never runs");
    assert_eq!(out.all().lines_matching("^    oops$"), 1, "it says why, with the end of the service's log");
    assert_eq!(out.all().lines_with("  exit 77  by=dibs"), 1, "and that dibs ended it");
}

#[test]
fn a_service_not_ready_in_time_ends_the_call_and_is_stopped() {
    let s = Sandbox::new();
    let srv = service(&s, "w3", "");
    let out = s.dibs(["--label", "with-slow", "--with", &format!("slow={srv}"), "--ready", "false", "--ready-within", "1", &format!("touch {}", s.p("w-ran"))]).run();
    assert_eq!((out.code, s.exists("w-ran")), (77, false), "one that is not ready in time ends the call with 77 too");
    assert!(!alive(pid_file(&s, "w3")), "and is stopped");
}

#[test]
fn a_service_that_exits_while_the_command_runs_stops_the_command() {
    let s = Sandbox::new();
    let (brief, never) = (s.gate("brief"), s.gate("never"));
    let srv = format!("brief=read -r -t 1 _ <> {}; exit 5", brief.path.display());
    let cmd = format!("{}; touch {}", never.hold(), s.p("w-ran"));
    let out = s.dibs(["--label", "with-mid", "--with", &srv, &cmd]).run();
    assert_eq!((out.code, s.exists("w-ran")), (77, false), "a service that exits while the command runs stops the command");
    assert_eq!(out.all().lines_matching("^  with brief: ready after [0-9]*s, exited 5 while the command ran"), 1, "and the trailer says so");
}

#[test]
fn status_names_a_running_service_under_its_job() {
    let mut s = Sandbox::new();
    let (up, go) = (s.gate("up"), s.gate("go"));
    let srv = service(&s, "w4", "");
    let job = s.spawn(s.dibs(["--label", "with-status", "--with", &format!("srv={srv}"), &format!("{}; {}", up.signal(), go.hold())]));
    up.reached();
    assert_eq!(s.status().lines_matching("^    with srv, pid [0-9]*: echo"), 1);
    go.open();
    s.wait(job);
}

#[test]
fn a_holds_command_here_runs_once_the_service_there_is_ready() {
    let s = Sandbox::new();
    let up = s.p("w5.up");
    let srv = service(&s, "w5", &format!("echo up > {up}; "));
    let out = s.dibs(["--hold", "--label", "with-hold", "--with", &format!("srv={srv}"), "--ready", &format!("test -s {up}"), &format!("cat {up}")]).run();
    assert_eq!(out.stdout, "up\n");
    assert_eq!(
        s.dibs(["--hold", "--device", "gpu:x", "--with", "srv=true", "true"]).run().all().lines_with("cannot be pinned"),
        0,
        "and a hold with a service may name the card the service runs on"
    );
}

#[test]
fn a_service_and_its_readiness_are_refused_where_they_mean_nothing() {
    let s = Sandbox::new();
    assert_eq!(s.dibs(["--peek", "--with", "srv=true", "true"]).code(), 2, "a peek takes no lock for a service to live under");
    assert_eq!(s.dibs(["--ready", "tcp:1", "true"]).code(), 2, "--ready belongs to a --with before it");
    assert_eq!(s.dibs(["--with", "./serve", "true"]).code(), 2, "and a service needs a name");
    assert_eq!(s.dibs(["--with", "srv=true", "--ready", "tcp:nope", "true"]).code(), 2, "--ready tcp: takes a number or a port name");
    assert_eq!(s.dibs(["--port", "8080", "true"]).code(), 2, "and --port takes a name to call it by");
}

#[test]
fn a_caller_that_dies_takes_its_service_with_it() {
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let srv = service(&s, "w6", "");
    let call = s.remote(s.dibs(["--label", "with-gone", "--with", &format!("srv={srv}"), &format!("{}; {}", up.signal(), never.hold())]));
    let caller = s.spawn(call);
    up.reached();
    unsafe { libc::kill(caller.pid as i32, libc::SIGKILL) };
    s.wait(caller);
    s.log_line("caller-gone.*with-gone");
    let pid = pid_file(&s, "w6");
    until("the service to stop", || !alive(pid));
    s.gone();
}

fn port_scripts(s: &Sandbox) -> (String, String) {
    // Both sides read the port out of the environment, so nothing in these commands names one.
    s.write(
        "pserve.py",
        "import os, signal, socket\ns = socket.socket()\ns.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\ns.bind((\"0.0.0.0\", int(os.environ[\"DIBS_PORT_API\"])))\ns.listen()\nsignal.pause()\n",
    );
    s.write(
        "pclient.py",
        "import socket, sys\nhost, _, port = sys.argv[1].rpartition(\":\")\nsocket.create_connection((host or \"127.0.0.1\", int(port)))\nprint(\"answered at\", sys.argv[1])\n",
    );
    (format!("srv=python3 {}", s.p("pserve.py")), format!("python3 {}", s.p("pclient.py")))
}

#[test]
fn a_service_and_a_command_agree_on_the_port_dibs_picked() {
    let s = Sandbox::new();
    let (srv, client) = port_scripts(&s);
    let out = s.dibs(["--label", "port-one", "--port", "api", "--with", &srv, "--ready", "tcp:api", &format!("{client} 127.0.0.1:$DIBS_PORT_API")]).run().all();
    let picked = capture(&out, r"^  port api: ([0-9]*) on ").unwrap();
    assert_eq!(out.lines_matching(&format!("^answered at 127.0.0.1:{picked}$")), 1);
}

#[test]
fn a_holds_command_is_told_where_to_reach_the_service() {
    let s = Sandbox::new();
    let (srv, client) = port_scripts(&s);
    let out = s.dibs(["--hold", "--label", "port-hold", "--port", "api", "--with", &srv, "--ready", "tcp:api", &format!("{client} \"$DIBS_SERVICE_API\"")]).run().stdout;
    let (at, port) = out.trim_end().rsplit_once(':').unwrap();
    assert_eq!(at, format!("answered at {}", hostname()), "by machine and port");
    assert!(port.parse::<u16>().is_ok(), "{out}");
}

#[test]
fn two_calls_at_once_are_never_given_the_same_port() {
    let mut s = Sandbox::new();
    let gates = [s.gate("go1"), s.gate("go2")];
    let jobs: Vec<Job> = (1..=2)
        .map(|i| {
            let cmd = format!("echo $DIBS_PORT_API > {}; {}", s.p(&format!("port{i}")), gates[i - 1].hold());
            s.spawn(s.dibs(["--label", &format!("port-race{i}"), "--port", "api", &cmd]))
        })
        .collect();
    until("both ports", || !s.read("port1").is_empty() && !s.read("port2").is_empty());
    assert_ne!(s.read("port1"), s.read("port2"), "two calls at once are never given the same port");
    assert_eq!(s.count("port"), 2, "and each port is reserved while it is held");
    for g in &gates {
        g.open();
    }
    for j in jobs {
        s.wait(j);
    }
    assert_eq!(s.count("port"), 0, "the reservation goes when the job does");
}

#[test]
fn a_port_in_use_is_not_handed_out() {
    let s = Sandbox::new();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let out = s.dibs(["--label", "port-none", "--port", "api", &format!("touch {}", s.p("port-ran"))]).env("DIBS_PORTS", format!("{port}-{port}")).run();
    assert_eq!((out.code, s.exists("port-ran")), (77, false), "a port in use is not handed out, and the call stops rather than colliding");
    assert_eq!(out.all().lines_with(&format!("no free port in {port}-{port}")), 1, "which says the range had nothing free");
}
