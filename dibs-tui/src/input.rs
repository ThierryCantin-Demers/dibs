//! What each key and mouse event does.

use std::{io::stdout, sync::mpsc::Sender};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, MouseButton, MouseEventKind},
    execute,
};

use crate::{
    action::Action,
    app::{App, Overlay, PendingKill},
    feed::Msg,
    text::dur,
};

const WHEEL_LINES: u16 = 3;
const PAGE_LINES: u16 = 15;

pub const HELP: &str = "\
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

impl App {
    /// True when the key means quit. Whatever is on top of the table gets the key first.
    pub fn on_key(&mut self, code: KeyCode, tx: &Sender<Msg>) -> bool {
        if let Some(kill) = self.confirm.take() {
            if matches!(code, KeyCode::Char('y' | 'Y')) {
                self.start_action(Action::kill(kill), tx);
            }
            return false;
        }
        if let Some(o) = self.overlay.as_mut() {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Char('j') | KeyCode::Down => o.scroll = o.scroll.saturating_add(1),
                KeyCode::Char('k') | KeyCode::Up => o.scroll = o.scroll.saturating_sub(1),
                KeyCode::Char('d') | KeyCode::PageDown => {
                    o.scroll = o.scroll.saturating_add(PAGE_LINES)
                }
                KeyCode::Char('u') | KeyCode::PageUp => {
                    o.scroll = o.scroll.saturating_sub(PAGE_LINES)
                }
                KeyCode::Char('g') => o.scroll = 0,
                _ => {}
            }
            return false;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') => self.sel = 0,
            KeyCode::Char('G') => self.sel = self.rows().len().saturating_sub(1),
            KeyCode::Char('?') => self.overlay = Some(Overlay::new("keys", HELP)),
            KeyCode::Char('L' | 'l') => self.start_action(Action::log(self.current_machine()), tx),
            KeyCode::Char('o') => self.show_output(tx),
            KeyCode::Char('n') => self.start_action(Action::gpu(self.current_machine()), tx),
            KeyCode::Char('p') | KeyCode::Enter => self.show_process_tree(tx),
            KeyCode::Char('K') => {
                self.confirm = self.selected().map(|it| PendingKill {
                    machine: it.machine,
                    pid: it.pid,
                    label: it.label,
                })
            }
            KeyCode::Char('m') => self.toggle_mouse(),
            KeyCode::Char('r') => self.start_action(Action::status(self.current_machine()), tx),
            KeyCode::Char('+' | '=') => self.set_interval((self.interval / 2).max(1), tx),
            KeyCode::Char('-' | '_') => self.set_interval(self.interval.saturating_mul(2), tx),
            KeyCode::Char(c @ '1'..='9') => {
                self.set_interval(c.to_digit(10).unwrap_or(2) as u64, tx)
            }
            _ => {}
        }
        false
    }

    /// Without mouse capture the terminal turns a wheel notch into three arrow keys, which
    /// on a list this short walks it end to end and reads as wrapping. One notch, one row.
    pub fn on_mouse(&mut self, kind: MouseEventKind, row: u16) {
        match kind {
            MouseEventKind::ScrollDown => match self.overlay.as_mut() {
                Some(o) => o.scroll = o.scroll.saturating_add(WHEEL_LINES),
                None => self.move_by(1),
            },
            MouseEventKind::ScrollUp => match self.overlay.as_mut() {
                Some(o) => o.scroll = o.scroll.saturating_sub(WHEEL_LINES),
                None => self.move_by(-1),
            },
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

    fn show_output(&mut self, tx: &Sender<Msg>) {
        let Some(it) = self.selected() else { return };
        if !it.holding {
            self.overlay = Some(Overlay::new(
                format!("{} has not started", it.label),
                "It is still queued, so it has written nothing yet.",
            ));
        } else if it.output.is_none() {
            self.overlay = Some(Overlay::new(
                format!("{} writes to no file", it.label),
                "Its output goes straight back to the agent that started it \
                 and is kept nowhere, so there is nothing here to read.\n\n\
                 A job that redirects into a file, which is what a recipe \
                 does for every step, can be read from here while it runs.",
            ));
        } else {
            self.start_action(Action::output(&it), tx);
        }
    }

    fn show_process_tree(&mut self, tx: &Sender<Msg>) {
        let Some(it) = self.selected() else { return };
        if it.holding {
            self.start_action(Action::process_tree(&it), tx);
            return;
        }
        let starts = it
            .eta
            .map(|e| format!("~{}", dur(e)))
            .unwrap_or_else(|| "no telling".into());
        self.overlay = Some(Overlay::new(
            format!("{} is still queued", it.label),
            format!(
                "It has no processes yet: it is waiting for the lock.\n\n\
                 waiting   {}\n starts in {starts}\n agent     {}\n\n{}",
                dur(it.time),
                it.agent,
                it.cmd
            ),
        ));
    }

    fn toggle_mouse(&mut self) {
        self.mouse = !self.mouse;
        let mut out = stdout();
        let _ = match self.mouse {
            true => execute!(out, EnableMouseCapture),
            false => execute!(out, DisableMouseCapture),
        };
    }
}
