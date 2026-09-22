//! A live view of the benchmark machine's lock, and a way to act on what is holding it.
//!
//! It owns no state of its own. `dibs --watch --json` is the feed, over the single
//! persistent connection that already exists, and every action shells back out to the same
//! wrapper, so this can never disagree with what `dibs --status` would have said.

mod action;
mod app;
mod feed;
mod input;
mod item;
mod status;
mod text;
mod ui;

use std::{
    io::stdout,
    process::exit,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
};

use crate::{app::App, feed::Msg};

const DEFAULT_INTERVAL: u64 = 10;
/// How long to wait for a key before looking at what the feeds sent.
const INPUT_POLL: Duration = Duration::from_millis(200);
/// sysexits' EX_UNAVAILABLE, which is what dibs itself exits with when it cannot be reached.
const EXIT_UNAVAILABLE: i32 = 69;

fn main() -> std::io::Result<()> {
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h" | "--help")) {
        println!("dibstop [seconds]   live view of the machine's lock, redrawing every {DEFAULT_INTERVAL}s by default");
        println!("{}", input::HELP);
        return Ok(());
    }
    let interval = app::interval(arg.and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_INTERVAL));

    let (tx, rx) = mpsc::channel();
    let mut app = App::start(feed::machines(), interval, &tx);
    if !app.has_feeds() {
        eprintln!("dibstop: could not start `dibs --watch --json`.");
        eprintln!("  It has to be on PATH; this is only a front end for it.");
        exit(EXIT_UNAVAILABLE);
    }

    let mut terminal = ratatui::init();
    // Capture costs the terminal's own click-to-select in this window; shift-drag still
    // reaches it, and a wheel that jumps three rows a notch is worse.
    let _ = execute!(stdout(), EnableMouseCapture);
    let res = run(&mut terminal, rx, tx, &mut app);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    // Each remote loop lives as long as its connection, and dropping the app stops them all.
    drop(app);
    res
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    app: &mut App,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(INPUT_POLL)? {
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
            app.receive(msg);
        }
    }
}
