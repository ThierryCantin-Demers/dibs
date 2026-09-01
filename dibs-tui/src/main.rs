//! A live view of the benchmark machine's lock, and a way to act on what is holding it.
//!
//! It owns no state of its own. `dibs --watch --json` is the feed, over the single
//! persistent connection that already exists, and every action shells back out to the same
//! wrapper, so this can never disagree with what `dibs --status` would have said.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap};
use serde::Deserialize;

// ---------------------------------------------------------------- the feed's shape

#[derive(Debug, Clone, Deserialize)]
struct Status {
    state: String,
    #[serde(default)]
    holders: Vec<Holder>,
    #[serde(default)]
    queue: Vec<Queued>,
}

#[derive(Debug, Clone, Deserialize)]
struct Holder {
    mode: String,
    pid: i64,
    label: String,
    agent: String,
    #[serde(default)]
    device: Option<String>,
    cmd: String,
    elapsed: i64,
    cpu: i64,
    #[serde(default)]
    cpu_rate: Option<i64>,
    est: Option<i64>,
    est_n: Option<i64>,
    est_scope: Option<String>,
    remaining: Option<i64>,
    overrun: Option<bool>,
    idle_for: Option<i64>,
    idle_kind: Option<String>,
    /// The file this job redirected into, when it redirected at all. Absent means `o` has
    /// nothing to show, and saying so beats offering a key that does nothing.
    #[serde(default)]
    output: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Queued {
    position: i64,
    mode: String,
    pid: i64,
    label: String,
    agent: String,
    #[serde(default)]
    device: Option<String>,
    cmd: String,
    waiting: i64,
    #[serde(default)]
    eta: Option<i64>,
}

enum Msg {
    State(u64, String, Status),
    Trouble(u64, String, String),
    Ended(u64, String, String),
    Action { title: String, body: String },
}

/// The machines to watch. Empty means there is no inventory, and the one feed goes wherever a
/// bare `dibs` would: a single machine looks exactly as it did before any of this existed.
fn machines() -> Vec<String> {
    let Ok(out) = Command::new("dibs").arg("--machines").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_machines(&String::from_utf8_lossy(&out.stdout))
}

/// `--machines` marks the default with a leading `*`, which is part of the line rather than
/// part of the name.
fn parse_machines(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            match it.next() {
                Some("*") => it.next(),
                other => other,
            }
            .map(str::to_string)
        })
        .collect()
}

// ---------------------------------------------------------------- shared vocabulary

/// The same hues the shell uses, in the same order, so one agent is one colour in both.
const HUES: [u8; 12] = [33, 39, 63, 99, 105, 135, 170, 176, 205, 38, 44, 111];

fn agent_hue(name: &str) -> Color {
    let mut h: u32 = 7;
    for c in name.chars() {
        h = h.wrapping_mul(31).wrapping_add(c as u32) & 0xffff;
    }
    Color::Indexed(HUES[h as usize % HUES.len()])
}

/// Columns are fixed width and ratatui cuts without saying so, which reads as a typo
/// rather than as a truncation. The full text is always in the pane below.
fn fit(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
    t.push('\u{2026}');
    t
}

/// Hundredths of a core, as cores. The total core-time goes in the pane below: on its own it
/// only ever prompts the question of why three minutes of work shows twenty-three of CPU.
fn cores(hundredths: i64) -> String {
    format!("{}.{}x", hundredths / 100, (hundredths % 100) / 10)
}

/// Matches the wrapper's own formatting, so numbers read the same in both places.
fn dur(s: i64) -> String {
    let s = s.max(0);
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

// ---------------------------------------------------------------- rows on screen

#[derive(Clone)]
struct Item {
    machine: String,
    /// Which card the job was pinned to, where one was named. Absent is the common case and
    /// is not a fault: a build does not want a card.
    device: Option<String>,
    holding: bool,
    slot: String,
    mode: String,
    pid: i64,
    label: String,
    agent: String,
    cmd: String,
    time: i64,
    cpu: Option<i64>,
    rate: Option<i64>,
    eta: Option<i64>,
    note: String,
    /// The column is narrow and the pane is not, so each says as much as it has room for.
    long: String,
    alarm: bool,
    output: Option<String>,
}

fn items(machine: &str, s: &Status) -> Vec<Item> {
    let mut v = Vec::new();
    for h in &s.holders {
        let note;
        let long;
        let mut alarm = false;
        if let Some(f) = h.idle_for {
            alarm = true;
            note = match h.idle_kind.as_deref() {
                Some("never") => format!("no cpu at all in {}", dur(f)),
                _ => format!("{} cpu, none in {}", dur(h.cpu), dur(f)),
            };
            long = match h.idle_kind.as_deref() {
                Some("never") => format!(
                    "It has burned no CPU at all in the {} since it acquired the lock: it is \
                     waiting on something. Stop it with K.",
                    dur(h.elapsed)
                ),
                _ => format!(
                    "It has burned {} of CPU in total, but none of it in the last {}, so it has \
                     stopped doing anything. Stop it with K.",
                    dur(h.cpu),
                    dur(f)
                ),
            };
        } else if h.overrun.unwrap_or(false) {
            alarm = true;
            note = format!("3x its usual {}", dur(h.est.unwrap_or(0)));
            long = format!(
                "It has been running {}, more than three times the {} this same job usually \
                 takes over {} runs.",
                dur(h.elapsed),
                dur(h.est.unwrap_or(0)),
                h.est_n.unwrap_or(0)
            );
        } else if let (Some(e), Some(n)) = (h.est, h.est_n) {
            let scope = h.est_scope.as_deref().unwrap_or("this");
            let usual = if e == 0 {
                "under a second".to_string()
            } else {
                dur(e)
            };
            note = match scope {
                "this" => format!("usually {usual} over {n} runs"),
                "agent" => format!("this agent: {usual} over {n} runs"),
                _ => format!("no history; {} \u{2248} {usual}", h.mode),
            };
            long = match scope {
                "this" => format!("This job usually takes {usual}, measured over {n} previous runs."),
                "agent" => format!(
                    "Nothing recorded under this label. This agent's other {} jobs take {usual} \
                     across {n} runs, which is the closest thing to an estimate there is.",
                    h.mode
                ),
                _ => format!(
                    "Nothing recorded for this job or this agent. {} runs on this machine take \
                     {usual} as a rule, which is worth knowing but says nothing about this one.",
                    h.mode
                ),
            };
        } else {
            note = "no history for this one yet".into();
            long = "Nothing recorded for this job or for its mode, so there is no honest \
                    estimate of how much longer it has."
                .into();
        }
        v.push(Item {
            machine: machine.to_string(),
            device: h.device.clone().filter(|d| d != "-" && !d.is_empty()),
            holding: true,
            slot: "HOLDING".into(),
            mode: h.mode.clone(),
            pid: h.pid,
            label: h.label.clone(),
            agent: h.agent.clone(),
            cmd: h.cmd.clone(),
            time: h.elapsed,
            cpu: Some(h.cpu),
            rate: h.cpu_rate,
            eta: h.remaining,
            note,
            long,
            alarm,
            output: h.output.clone(),
        });
    }
    for q in &s.queue {
        v.push(Item {
            machine: machine.to_string(),
            device: q.device.clone().filter(|d| d != "-" && !d.is_empty()),
            holding: false,
            slot: format!("queued {}", q.position),
            mode: q.mode.clone(),
            pid: q.pid,
            label: q.label.clone(),
            agent: q.agent.clone(),
            cmd: q.cmd.clone(),
            time: q.waiting,
            cpu: None,
            rate: None,
            eta: q.eta,
            note: match q.eta {
                Some(e) => format!("starts in ~{}", dur(e)),
                None => "no telling when".into(),
            },
            long: match q.eta {
                Some(e) => format!(
                    "Waiting for the lock. Nothing ahead of it is expected to take more than {}.",
                    dur(e)
                ),
                None => "Waiting for the lock. Something ahead of it is already past its usual \
                         duration, so there is no honest estimate of when this one starts."
                    .into(),
            },
            alarm: false,
            output: None,   // it has no processes yet, so nothing to write with
        });
    }
    v
}

// ---------------------------------------------------------------- talking to the wrapper

const TREE: &str = r#"ps -eo pid=,ppid=,stat=,etime=,time=,args= | awk -v r=PIDHERE '{p[NR]=$1;q[NR]=$2;l[NR]=$0} END {w[r]=1; do {c=0; for(i=1;i<=NR;i++) if(w[q[i]]&&!w[p[i]]){w[p[i]]=1;c=1}} while(c); for(i=1;i<=NR;i++) if(w[p[i]]) print substr(l[i],1,200)}'"#;

/// A job's output is whatever the job printed, and build tools print two things this cannot
/// render: colour escapes, and the carriage returns a progress line redraws itself with.
/// Passed through, the first corrupts the terminal rather than the paragraph, and the second
/// makes one line look like several overlaid.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.peek() {
                // CSI, terminated by a byte in @ through ~. Colour is all of these here.
                Some('[') => {
                    it.next();
                    for c2 in it.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            break;
                        }
                    }
                }
                // OSC, terminated by BEL or ST. Terminal titles, mostly.
                Some(']') => {
                    it.next();
                    for c2 in it.by_ref() {
                        if c2 == '\u{7}' || c2 == '\u{1b}' {
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            },
            '\r' => {}
            '\t' => out.push_str("    "),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

fn act(tx: Sender<Msg>, machine: String, title: String, args: Vec<String>) {
    thread::spawn(move || {
        let mut cmd = Command::new("dibs");
        if !machine.is_empty() {
            cmd.arg("--on").arg(&machine);
        }
        let body = match cmd.args(&args).output() {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                let e = String::from_utf8_lossy(&o.stderr);
                if !e.trim().is_empty() {
                    if !s.is_empty() {
                        s.push('\n');
                    }
                    s.push_str(&e);
                }
                if s.trim().is_empty() {
                    s = format!("(nothing, exit {})", o.status.code().unwrap_or(-1));
                }
                s
            }
            Err(e) => format!("could not run dibs: {e}"),
        };
        let _ = tx.send(Msg::Action { title, body: plain(&body) });
    });
}

fn spawn_feed(
    tx: Sender<Msg>,
    machine: String,
    interval: u64,
    gen: u64,
) -> std::io::Result<Arc<Mutex<Child>>> {
    // Under setpriv so the feed dies with this process however this process dies. Killing the
    // feeds on the way out only covers the ways out that run code; a SIGKILL, or a terminal
    // closing on the whole thing, leaves each feed reparented to init and polling a shared
    // machine every couple of seconds until somebody notices. Four of those ran for four hours
    // before anyone did. `dibs` already relies on the same tool to make its ssh client die with
    // it, so this is the same guarantee one level up.
    let mut args: Vec<String> = Vec::new();
    if !machine.is_empty() {
        args.push("--on".into());
        args.push(machine.clone());
    }
    args.extend(["--watch".to_string(), interval.to_string(), "--json".to_string()]);

    let spawn = |wrapped: bool| {
        let mut cmd = if wrapped {
            let mut c = Command::new("setpriv");
            c.arg("--pdeathsig=TERM").arg("dibs");
            c
        } else {
            Command::new("dibs")
        };
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    // Without setpriv the feed still works; it just outlives a hard kill, which is the state
    // this was in before and is better than not running at all.
    let mut child = match spawn(true) {
        Ok(c) => c,
        Err(_) => spawn(false)?,
    };
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    let t = tx.clone();
    let m = machine.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.starts_with('{') {
                continue;
            }
            match serde_json::from_str::<Status>(&line) {
                Ok(s) => {
                    if t.send(Msg::State(gen, m.clone(), s)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ =
                        t.send(Msg::Trouble(gen, m.clone(), format!("unreadable document: {e}")));
                }
            }
        }
        let _ = t.send(Msg::Ended(gen, m, "the feed closed".into()));
    });

    thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        let s = s.trim();
        if !s.is_empty() {
            let _ = tx.send(Msg::Trouble(gen, machine.clone(), s.to_string()));
        }
    });

    Ok(Arc::new(Mutex::new(child)))
}

// ---------------------------------------------------------------- the app

struct Overlay {
    title: String,
    body: String,
    scroll: u16,
}

/// One machine's side of the world. Held apart rather than merged, because "the feed is down"
/// and "it is idle" are different answers and a merged view can only give one of them.
#[derive(Default)]
struct View {
    status: Option<Status>,
    seen_at: Option<Instant>,
    trouble: Option<String>,
    dead: Option<String>,
}

struct App {
    views: std::collections::BTreeMap<String, View>,
    sel: usize,
    top: usize,
    overlay: Option<Overlay>,
    confirm: Option<(String, i64, String)>,
    busy: Option<String>,
    interval: u64,
    feeds: Vec<(String, Arc<Mutex<Child>>)>,
    gen: u64,
    /// Where the first job landed on screen last frame, so a click knows what it hit.
    table_top: u16,
    table_rows: u16,
    /// Capturing the mouse takes the terminal's own text selection away, which is worth
    /// having back sometimes. The wheel and clicks go with it while it is off.
    mouse: bool,
}

impl App {
    fn rows(&self) -> Vec<Item> {
        self.views
            .iter()
            .filter_map(|(m, v)| v.status.as_ref().map(|s| items(m, s)))
            .collect::<Vec<_>>()
            .concat()
    }

    /// Whose log, whose GPU, whose lock directory. The row under the cursor answers it; with
    /// nothing selected the first machine does, which is the only one when there is one.
    fn current_machine(&self) -> String {
        self.rows()
            .get(self.sel)
            .map(|it| it.machine.clone())
            .or_else(|| self.views.keys().next().cloned())
            .unwrap_or_default()
    }

    /// The column is worth its width only when there is more than one machine to tell apart.
    fn multi(&self) -> bool {
        self.views.len() > 1
    }

    /// The interval lives in the feed, so changing it means a new one. The generation
    /// counter is what stops the old feed's closing breath from being read as this one
    /// dying.
    fn set_interval(&mut self, secs: u64, tx: &Sender<Msg>) {
        let secs = secs.clamp(1, 60);
        if secs == self.interval {
            return;
        }
        self.interval = secs;
        self.gen += 1;
        for (_, c) in &self.feeds {
            if let Ok(mut c) = c.lock() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        let names: Vec<String> = self.feeds.iter().map(|(m, _)| m.clone()).collect();
        self.feeds.clear();
        for m in names {
            match spawn_feed(tx.clone(), m.clone(), self.interval, self.gen) {
                Ok(c) => {
                    self.feeds.push((m.clone(), c));
                    self.views.entry(m).or_default().dead = None;
                }
                Err(e) => {
                    self.views.entry(m).or_default().dead =
                        Some(format!("could not restart the feed: {e}"))
                }
            }
        }
        self.busy = Some(format!("reconnecting every {secs}s"));
    }

    fn move_by(&mut self, delta: i32) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let next = (self.sel as i32 + delta).clamp(0, n as i32 - 1);
        self.sel = next as usize;
    }

    /// Without mouse capture the terminal turns a wheel notch into three arrow keys, which
    /// on a list this short walks it end to end and reads as wrapping. One notch, one row.
    fn on_mouse(&mut self, kind: MouseEventKind, row: u16) {
        match kind {
            MouseEventKind::ScrollDown => {
                if let Some(o) = self.overlay.as_mut() {
                    o.scroll = o.scroll.saturating_add(3);
                } else {
                    self.move_by(1);
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(o) = self.overlay.as_mut() {
                    o.scroll = o.scroll.saturating_sub(3);
                } else {
                    self.move_by(-1);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.overlay.is_some() || self.confirm.is_some() {
                    return;
                }
                if row >= self.table_top && row < self.table_top + self.table_rows {
                    let hit = self.top + (row - self.table_top) as usize;
                    if hit < self.rows().len() {
                        self.sel = hit;
                    }
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, code: KeyCode, tx: &Sender<Msg>) -> bool {
        // Whatever is on top gets the key first.
        if self.confirm.is_some() {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let (machine, pid, label) = self.confirm.take().unwrap();
                    self.busy = Some(format!("stopping {label}"));
                    act(
                        tx.clone(),
                        machine,
                        format!("kill {pid} ({label})"),
                        vec!["--kill".into(), pid.to_string()],
                    );
                }
                _ => self.confirm = None,
            }
            return false;
        }
        if let Some(o) = self.overlay.as_mut() {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Char('j') | KeyCode::Down => o.scroll = o.scroll.saturating_add(1),
                KeyCode::Char('k') | KeyCode::Up => o.scroll = o.scroll.saturating_sub(1),
                KeyCode::Char('d') | KeyCode::PageDown => o.scroll = o.scroll.saturating_add(15),
                KeyCode::Char('u') | KeyCode::PageUp => o.scroll = o.scroll.saturating_sub(15),
                KeyCode::Char('g') => o.scroll = 0,
                _ => {}
            }
            return false;
        }

        let rows = self.rows();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') => self.sel = 0,
            KeyCode::Char('G') => self.sel = rows.len().saturating_sub(1),
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay {
                    title: "keys".into(),
                    body: HELP.into(),
                    scroll: 0,
                })
            }
            KeyCode::Char('L') | KeyCode::Char('l') => {
                self.busy = Some("reading the log".into());
                act(
                    tx.clone(),
                    self.current_machine(),
                    "recent arrivals and outcomes".into(),
                    vec!["--log".into(), "40".into()],
                );
            }
            KeyCode::Char('o') => {
                if let Some(it) = rows.get(self.sel) {
                    if it.holding && it.output.is_none() {
                        self.overlay = Some(Overlay {
                            title: format!("{} writes to no file", it.label),
                            body: "Its output goes straight back to the agent that started it \
                                   and is kept nowhere, so there is nothing here to read.\n\n\
                                   A job that redirects into a file, which is what dibs-run \
                                   does for every step, can be read from here while it runs."
                                .into(),
                            scroll: 0,
                        });
                    } else if it.holding {
                        self.busy = Some(format!("reading what {} is writing", it.pid));
                        act(
                            tx.clone(),
                            it.machine.clone(),
                            format!("output of {} ({})", it.pid, it.label),
                            vec!["--out".into(), it.pid.to_string()],
                        );
                    } else {
                        self.overlay = Some(Overlay {
                            title: format!("{} has not started", it.label),
                            body: "It is still queued, so it has written nothing yet.".into(),
                            scroll: 0,
                        });
                    }
                }
            }
            KeyCode::Char('n') => {
                self.busy = Some("asking the GPU".into());
                act(
                    tx.clone(),
                    self.current_machine(),
                    "nvidia-smi".into(),
                    vec!["--peek".into(), "nvidia-smi".into()],
                );
            }
            KeyCode::Char('p') | KeyCode::Enter => {
                if let Some(it) = rows.get(self.sel) {
                    if it.holding {
                        self.busy = Some(format!("looking at {}", it.pid));
                        act(
                            tx.clone(),
                            it.machine.clone(),
                            format!("process tree under {} ({})", it.pid, it.label),
                            vec!["--peek".into(), TREE.replace("PIDHERE", &it.pid.to_string())],
                        );
                    } else {
                        self.overlay = Some(Overlay {
                            title: format!("{} is still queued", it.label),
                            body: format!(
                                "It has no processes yet: it is waiting for the lock.\n\n\
                                 waiting   {}\n starts in {}\n agent     {}\n\n{}",
                                dur(it.time),
                                it.eta
                                    .map(|e| format!("~{}", dur(e)))
                                    .unwrap_or_else(|| "no telling".into()),
                                it.agent,
                                it.cmd
                            ),
                            scroll: 0,
                        });
                    }
                }
            }
            KeyCode::Char('K') => {
                if let Some(it) = rows.get(self.sel) {
                    self.confirm = Some((it.machine.clone(), it.pid, it.label.clone()));
                }
            }
            KeyCode::Char('m') => {
                self.mouse = !self.mouse;
                let mut out = std::io::stdout();
                let _ = if self.mouse {
                    execute!(out, EnableMouseCapture)
                } else {
                    execute!(out, DisableMouseCapture)
                };
            }
            KeyCode::Char('r') => {
                self.busy = Some("refreshing".into());
                act(tx.clone(), self.current_machine(), "status".into(), vec!["--status".into()]);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                let n = (self.interval / 2).max(1);
                self.set_interval(n, tx);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                let n = self.interval.saturating_mul(2);
                self.set_interval(n, tx);
            }
            KeyCode::Char(c @ '1'..='9') => {
                let n = c.to_digit(10).unwrap_or(2) as u64;
                self.set_interval(n, tx);
            }
            _ => {}
        }
        false
    }
}

const HELP: &str = "\
  j / k        move down and up
  g / G        first and last
  p or Enter   the selected job's process tree, through --peek
  o            what the selected job is writing, if it redirected to a file
  n            nvidia-smi on the machine
  L            recent arrivals and outcomes
  K            stop the selected job, with a confirmation
  r            run --status once and show what it prints
  m            hand the mouse back to the terminal, and take it again
  wheel        one row a notch, or scrolls whatever is open over the top
  click        select a job
  + / -        redraw twice as often, or half as often
  1 … 9        redraw every that many seconds
  ?            this
  q            quit

A job's output goes straight back to the agent that started it and is kept
nowhere, so o can only show a job that redirected into a file. That is what
agents mostly do, and the file is found by asking the kernel where the job's
open descriptors point, so nothing has to be arranged in advance.

While this has the mouse, the terminal cannot do its own text selection. Press m
to hand it back, and the wheel goes back to whatever your terminal does with it.
Shift and drag usually reaches the terminal's selection without letting go.

The interval is the feed's, not this window's, so changing it opens a new
connection. Every tick costs a lock read on the far side, which is little
enough that one a second is affordable, but the machine is shared: a person
reads a queue about as fast at two seconds as at one.

Everything here shells out to the same wrapper the agents use, so nothing
this shows can disagree with what dibs --status would have said.

The feed is one persistent connection: a redraw costs a lock read on the
far side, not a fresh login, so leaving this open beside a benchmark is
about as expensive as not leaving it open.";

// ---------------------------------------------------------------- drawing

/// The same two colours the header uses for the machine's state, so a row reads the same
/// way as the line summarising it.
fn mode_style(mode: &str, base: Style) -> Style {
    base.fg(if mode == "bench" { Color::Red } else { Color::Yellow })
        .add_modifier(Modifier::BOLD)
}

fn state_style(state: &str) -> (String, Style) {
    match state {
        "bench" => (
            "BUSY, benchmark in progress".into(),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        "shared" => (
            "in use, shared".into(),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        "idle" => (
            "idle".into(),
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        "orphan" => (
            "LOCKED BY AN ORPHAN".into(),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        other => (other.into(), Style::new()),
    }
}

fn centred(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let h = area.height * pct_y / 100;
    let w = area.width * pct_x / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let dim = Style::new().fg(Color::DarkGray);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(8),
        Constraint::Length(1),
    ])
    .split(f.area());

    // --- header -------------------------------------------------------------
    let rows = app.rows();
    let mut head: Vec<Span> = Vec::new();
    // One machine per group rather than a single verdict: "the feed is down" and "it is idle"
    // are different answers, and a summary across machines can only give one of them.
    let multi = app.multi();
    let mut oldest: Option<u64> = None;
    for (name, v) in &app.views {
        if !head.is_empty() {
            head.push(Span::styled("   ", dim));
        }
        if multi {
            head.push(Span::styled(format!("{name} "), dim));
        }
        match (&v.dead, &v.status) {
            (Some(d), _) => head.push(Span::styled(
                if multi { "down".to_string() } else { format!("feed down: {d}") },
                Style::new().fg(Color::Red),
            )),
            (None, Some(s)) => {
                let (text, style) = state_style(&s.state);
                head.push(Span::styled(
                    if multi { text.to_string() } else { format!("dibs: {text}") },
                    style,
                ));
                if !s.queue.is_empty() {
                    head.push(Span::styled(format!(" ({} queued)", s.queue.len()), dim));
                }
            }
            (None, None) => head.push(Span::styled("connecting…", dim)),
        }
        if let Some(t) = v.seen_at {
            let age = t.elapsed().as_secs();
            oldest = Some(oldest.map_or(age, |o: u64| o.max(age)));
        }
    }
    if let Some(age) = oldest {
        let stale = age > app.interval * 3 + 2;
        head.push(Span::styled(
            format!("   updated {age}s ago, every {}s", app.interval),
            if stale { Style::new().fg(Color::Yellow) } else { dim },
        ));
    }
    if let Some(b) = &app.busy {
        head.push(Span::styled(format!("  · {b}…"), Style::new().fg(Color::Cyan)));
    }
    f.render_widget(Paragraph::new(Line::from(head)), chunks[0]);

    // --- table --------------------------------------------------------------
    let body_h = chunks[1].height.saturating_sub(3) as usize;
    if app.sel >= rows.len() {
        app.sel = rows.len().saturating_sub(1);
    }
    if app.sel < app.top {
        app.top = app.sel;
    } else if body_h > 0 && app.sel >= app.top + body_h {
        app.top = app.sel + 1 - body_h;
    }

    app.table_top = chunks[1].y + 2; // border, then the header row
    app.table_rows = body_h as u16;
    let any_device = rows.iter().any(|it| it.device.is_some());
    let mut trows: Vec<Row> = Vec::new();
    for (i, it) in rows.iter().enumerate().skip(app.top).take(body_h.max(1)) {
        let hue = agent_hue(&it.agent);
        let sel = i == app.sel;
        let base = if sel {
            Style::new().bg(Color::Indexed(236)).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let slot_style = if it.holding {
            base.fg(if it.alarm { Color::Yellow } else { Color::White })
        } else {
            base.fg(Color::Cyan)
        };
        let mut cells = vec![Span::styled(
            if sel { "▸" } else { " " }.to_string(),
            base.fg(hue),
        )];
        if multi {
            cells.push(Span::styled(fit(&it.machine, 15), base.patch(dim)));
        }
        cells.extend([
                Span::styled(it.slot.clone(), slot_style),
                Span::styled(it.mode.clone(), mode_style(&it.mode, base)),
                Span::styled(fit(&it.label, 18), base.fg(hue)),
        ]);
        if any_device {
            // An unpinned job among pinned ones is the thing worth seeing, so it reads as a
            // dash rather than as blank space.
            cells.push(match &it.device {
                Some(d) => Span::styled(fit(d, 16), base.fg(Color::Magenta)),
                None => Span::styled(fit("-", 16), base.patch(dim)),
            });
        }
        cells.extend([
                Span::styled(fit(&it.agent, 20), base.fg(hue)),
                Span::styled(dur(it.time), base.add_modifier(Modifier::BOLD)),
                Span::styled(
                    it.rate.map(cores).unwrap_or_else(|| "-".into()),
                    base.patch(dim),
                ),
                Span::styled(
                    it.note.clone(),
                    if it.alarm {
                        base.fg(Color::Yellow)
                    } else {
                        base.patch(dim)
                    },
                ),
        ]);
        trows.push(Row::new(cells).style(base));
    }
    if trows.is_empty() {
        trows.push(Row::new(vec![Span::styled(
            "  nothing holding it, nothing queued".to_string(),
            Style::new().fg(Color::Green),
        )]));
    }
    let mut widths = vec![Constraint::Length(1)];
    let mut heads = vec![""];
    if multi {
        widths.push(Constraint::Length(16));
        heads.push("MACHINE");
    }
    widths.extend([
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(18),
    ]);
    heads.extend(["WHAT", "MODE", "LABEL"]);
    if any_device {
        widths.push(Constraint::Length(17));
        heads.push("DEVICE");
    }
    widths.extend([
        Constraint::Length(20),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Min(24),
    ]);
    heads.extend(["AGENT", "TIME", "CORES", "NOTE"]);
    let table = Table::new(trows, widths)
    .header(
        Row::new(heads)
            .style(dim.add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(" jobs "));
    f.render_widget(table, chunks[1]);

    // --- detail -------------------------------------------------------------
    let detail = match rows.get(app.sel) {
        Some(it) => {
            let hue = agent_hue(&it.agent);
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("from  ", dim),
                    Span::styled(it.agent.clone(), Style::new().fg(hue)),
                    Span::styled(format!("   pid {}", it.pid), dim),
                ]),
                Line::from(Span::styled(it.cmd.clone(), Style::new())),
            ];
            if it.holding {
                lines.push(Line::from(vec![
                    Span::styled("running ", dim),
                    Span::raw(dur(it.time)),
                    Span::styled("   cpu ", dim),
                    Span::raw(match it.rate {
                        Some(r) => format!(
                            "{} across all cores, {} right now",
                            dur(it.cpu.unwrap_or(0)),
                            cores(r)
                        ),
                        None => format!("{} across all cores", dur(it.cpu.unwrap_or(0))),
                    }),
                    Span::styled("   left ", dim),
                    Span::raw(
                        it.eta
                            .map(|e| format!("~{}", dur(e)))
                            .unwrap_or_else(|| "unknown".into()),
                    ),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("waiting ", dim),
                    Span::raw(dur(it.time)),
                    Span::styled("   starts in ", dim),
                    Span::raw(match it.eta {
                        Some(e) => format!("~{}", dur(e)),
                        None => "no telling".into(),
                    }),
                ]));
            }
            // Where its output is going, so `o` is discovered by looking rather than by
            // already knowing. A job that redirected nowhere gets nothing here, because a key
            // offered for a job it cannot read is worse than no offer.
            if let Some(out) = &it.output {
                lines.push(Line::from(vec![
                    Span::styled("writing ", dim),
                    Span::raw(out.clone()),
                    Span::styled("   press o", dim),
                ]));
            }
            lines.push(Line::from(Span::styled(
                if it.alarm {
                    format!("\u{26a0} {}", it.long)
                } else {
                    it.long.clone()
                },
                if it.alarm {
                    Style::new().fg(Color::Yellow)
                } else {
                    dim
                },
            )));
            Paragraph::new(lines).wrap(Wrap { trim: false })
        }
        None => Paragraph::new(Span::styled(
            app.views
                .values()
                .filter_map(|v| v.trouble.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            Style::new().fg(Color::Yellow),
        )),
    };
    f.render_widget(
        detail.block(Block::default().borders(Borders::ALL).title(" selected ")),
        chunks[2],
    );

    // --- footer -------------------------------------------------------------
    f.render_widget(
        Paragraph::new(Span::styled(
                        format!(
                "  j/k move   p tree   o output   n gpu   L log   K kill   +/- rate   m mouse:{}   ? keys   q quit",
                if app.mouse { "on" } else { "off" }
            ),
            dim,
        )),
        chunks[3],
    );

    // --- overlays -----------------------------------------------------------
    if let Some(o) = &app.overlay {
        let area = centred(f.area(), 86, 84);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(o.body.clone())
                .scroll((o.scroll, 0))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {} ", o.title))
                        .title_bottom(" j/k scroll   esc close "),
                ),
            area,
        );
    }
    if let Some((_, pid, label)) = &app.confirm {
        let area = centred(f.area(), 56, 22);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Stop {label} (pid {pid})?"),
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  It is someone's work. y to send SIGTERM, anything else cancels.",
                    Style::new().fg(Color::DarkGray),
                )),
            ])
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(Color::Red))
                    .title(" confirm "),
            ),
            area,
        );
    }
}

// ---------------------------------------------------------------- loop

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    app: &mut App,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.on_key(k.code, &tx) {
                        return Ok(());
                    }
                }
                Event::Mouse(m) => app.on_mouse(m.kind, m.row),
                _ => {}
            }
        }
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::State(g, m, s) if g == app.gen => {
                    let v = app.views.entry(m).or_default();
                    v.status = Some(s);
                    v.seen_at = Some(Instant::now());
                    v.dead = None;
                    app.busy = None;
                }
                Msg::Trouble(g, m, t) if g == app.gen => {
                    app.views.entry(m).or_default().trouble = Some(t)
                }
                Msg::Ended(g, m, e) if g == app.gen => {
                    app.views.entry(m).or_default().dead = Some(e)
                }
                Msg::State(..) | Msg::Trouble(..) | Msg::Ended(..) => {}
                Msg::Action { title, body } => {
                    app.busy = None;
                    app.overlay = Some(Overlay {
                        title,
                        body,
                        scroll: 0,
                    });
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h") | Some("--help")) {
        println!("dibstop [seconds]   live view of the machine's lock, redrawing every 2s");
        println!("{HELP}");
        return Ok(());
    }
    let interval: u64 = arg.and_then(|s| s.parse().ok()).unwrap_or(2).clamp(1, 60);

    let (tx, rx) = mpsc::channel();
    // No inventory means one unnamed feed, going wherever a bare `dibs` would, so a single
    // machine looks exactly as it did before there was more than one.
    let names = {
        let m = machines();
        if m.is_empty() { vec![String::new()] } else { m }
    };
    let mut feeds = Vec::new();
    let mut views = std::collections::BTreeMap::new();
    for m in &names {
        views.insert(m.clone(), View::default());
        match spawn_feed(tx.clone(), m.clone(), interval, 0) {
            Ok(c) => feeds.push((m.clone(), c)),
            Err(e) => {
                views.get_mut(m).unwrap().dead = Some(format!("could not start: {e}"));
            }
        }
    }
    if feeds.is_empty() {
        eprintln!("dibstop: could not start `dibs --watch --json`.");
        eprintln!("  It has to be on PATH; this is only a front end for it.");
        std::process::exit(69);
    }

    let mut app = App {
        views,
        sel: 0,
        top: 0,
        overlay: None,
        confirm: None,
        busy: None,
        interval,
        feeds,
        gen: 0,
        table_top: 0,
        table_rows: 0,
        mouse: true,
    };

    let mut terminal = ratatui::init();
    // Capture costs the terminal's own click-to-select in this window; shift-drag still
    // reaches it, and a wheel that jumps three rows a notch is worse.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = run(&mut terminal, rx, tx, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    // Each remote loop lives as long as its connection, so they all go with us.
    for (_, c) in &app.feeds {
        if let Ok(mut c) = c.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_and_progress_redraws_do_not_reach_the_screen() {
        let cargo = "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m cubecl v0.1\n\
                     \r    Blocking waiting for file\rok\n";
        assert_eq!(plain(cargo), "   Compiling cubecl v0.1\n    Blocking waiting for fileok\n");
    }

    #[test]
    fn newlines_survive_because_the_paragraph_needs_them() {
        assert_eq!(plain("a\nb\tc\u{0}d"), "a\nb    cd");
    }

    #[test]
    fn the_default_marker_is_not_part_of_the_name() {
        let listing = " * bench1 dibs@bench1\n   laptop     laptop  (no measurements)\n";
        assert_eq!(parse_machines(listing), vec!["bench1", "laptop"]);
    }

    #[test]
    fn no_inventory_is_no_machines_rather_than_one_empty_name() {
        assert!(parse_machines("").is_empty());
    }
}
