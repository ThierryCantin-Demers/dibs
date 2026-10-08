//! The screens drawn for a recorded feed, against the ones kept beside it. `UPDATE_FRAMES=1`
//! writes them anew.

use crate::{app::App, feed::Msg, ui};
use ratatui::{Terminal, backend::TestBackend};
use std::path::PathBuf;

const FEED: &str = include_str!("../fixtures/feed.jsonl");
const COLUMNS: u16 = 120;
const ROWS: u16 = 30;

fn recorded() -> App {
    let mut app = App::new(10);
    for line in FEED.lines() {
        let (machine, document) = line.split_once(' ').expect("a machine, then its document");
        app.receive(Msg::State {
            generation: 0,
            machine: machine.to_string(),
            status: serde_json::from_str(document).expect("a status document"),
        });
    }
    app
}

fn drawn(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(COLUMNS, ROWS)).expect("a test terminal");
    terminal
        .draw(|f| ui::draw(f, app))
        .expect("a frame is drawn");
    let buffer = terminal.backend().buffer();
    (0..ROWS)
        .map(|y| {
            let row: String = (0..COLUMNS)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            format!("{}\n", row.trim_end())
        })
        .collect()
}

fn kept(name: &str, frame: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("frame-{name}.txt"));
    if std::env::var_os("UPDATE_FRAMES").is_some() {
        std::fs::write(&path, frame).expect("the frame is written");
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        frame,
        want,
        "frame {name} differs from {}; UPDATE_FRAMES=1 accepts it",
        path.display()
    );
}

fn selecting(app: &mut App, label: &str) {
    app.sel = app
        .rows()
        .iter()
        .position(|it| it.label.as_str() == label)
        .expect("the label is in the feed");
}

#[test]
fn two_machines_are_drawn_as_kept() {
    kept("machines", &drawn(&mut recorded()));
}

#[test]
fn an_overrun_says_what_it_was_measured_against() {
    let mut app = recorded();
    selecting(&mut app, "bench app gemv");
    kept("overrun", &drawn(&mut app));
}

#[test]
fn a_step_of_a_batch_says_what_comes_after_it() {
    let mut app = recorded();
    selecting(&mut app, "build lib");
    kept("batch", &drawn(&mut app));
}

#[test]
fn an_idle_holder_is_told_apart() {
    let mut app = recorded();
    selecting(&mut app, "test lib");
    kept("idle", &drawn(&mut app));
}
