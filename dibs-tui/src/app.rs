//! What the screen shows and what the keys act on: one view per machine, the selection, and
//! whatever is open over the table.

use std::{collections::BTreeMap, sync::mpsc::Sender, time::Instant};

use crate::{
    action::Action,
    feed::{Feed, Msg},
    item::Item,
    status::Status,
};

const MIN_INTERVAL: u64 = 1;
const MAX_INTERVAL: u64 = 60;

pub fn interval(secs: u64) -> u64 {
    secs.clamp(MIN_INTERVAL, MAX_INTERVAL)
}

pub struct Overlay {
    pub title: String,
    pub body: String,
    pub scroll: u16,
}

impl Overlay {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Overlay {
        Overlay {
            title: title.into(),
            body: body.into(),
            scroll: 0,
        }
    }
}

/// A kill waiting for its `y`.
pub struct PendingKill {
    pub machine: String,
    pub pid: i64,
    pub label: String,
}

/// One machine's side of the world. Held apart rather than merged, because "the feed is down"
/// and "it is idle" are different answers and a merged view can only give one of them.
#[derive(Default)]
pub struct View {
    pub status: Option<Status>,
    pub seen_at: Option<Instant>,
    pub trouble: Option<String>,
    pub dead: Option<String>,
}

pub struct App {
    pub views: BTreeMap<String, View>,
    pub sel: usize,
    pub top: usize,
    pub overlay: Option<Overlay>,
    pub confirm: Option<PendingKill>,
    pub busy: Option<String>,
    pub interval: u64,
    feeds: Vec<Feed>,
    generation: u64,
    /// Where the first job landed on screen last frame, so a click knows what it hit.
    pub table_top: u16,
    pub table_rows: u16,
    /// Capturing the mouse takes the terminal's own text selection away, which is worth
    /// having back sometimes. The wheel and clicks go with it while it is off.
    pub mouse: bool,
    /// When the round now being collected began, and when the last complete one ended. A round
    /// is every live feed having reported, which is the only moment the whole screen was current.
    round_from: Instant,
    pub refreshed: Option<Instant>,
}

impl App {
    fn new(interval: u64) -> App {
        App {
            views: BTreeMap::new(),
            sel: 0,
            top: 0,
            overlay: None,
            confirm: None,
            busy: None,
            interval,
            feeds: Vec::new(),
            generation: 0,
            table_top: 0,
            table_rows: 0,
            mouse: true,
            round_from: Instant::now(),
            refreshed: None,
        }
    }

    /// One feed per machine. No inventory means one unnamed feed, going wherever a bare `dibs`
    /// would, so a single machine looks exactly as it did before there was more than one.
    pub fn start(machines: Vec<String>, interval: u64, tx: &Sender<Msg>) -> App {
        let names = if machines.is_empty() {
            vec![String::new()]
        } else {
            machines
        };
        let mut app = App::new(interval);
        for m in names {
            let view = app.views.entry(m.clone()).or_default();
            match Feed::spawn(tx.clone(), m, interval, app.generation) {
                Ok(feed) => app.feeds.push(feed),
                Err(e) => view.dead = Some(format!("could not start: {e}")),
            }
        }
        app
    }

    pub fn has_feeds(&self) -> bool {
        !self.feeds.is_empty()
    }

    pub fn rows(&self) -> Vec<Item> {
        self.views
            .iter()
            .filter_map(|(m, v)| v.status.as_ref().map(|s| Item::all(m, s)))
            .flatten()
            .collect()
    }

    pub fn selected(&self) -> Option<Item> {
        self.rows().into_iter().nth(self.sel)
    }

    /// Whose log, whose GPU, whose lock directory. The row under the cursor answers it; with
    /// nothing selected the first machine does, which is the only one when there is one.
    pub fn current_machine(&self) -> String {
        self.selected()
            .map(|it| it.machine)
            .or_else(|| self.views.keys().next().cloned())
            .unwrap_or_default()
    }

    /// The column is worth its width only when there is more than one machine to tell apart.
    pub fn multi(&self) -> bool {
        self.views.len() > 1
    }

    pub fn start_action(&mut self, action: Action, tx: &Sender<Msg>) {
        self.busy = Some(action.doing.clone());
        action.start(tx.clone());
    }

    pub fn move_by(&mut self, delta: i32) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel as i32 + delta).clamp(0, n as i32 - 1) as usize;
    }

    /// The interval lives in the feed, so changing it means a new one. The generation
    /// counter is what stops the old feed's closing breath from being read as this one
    /// dying.
    pub fn set_interval(&mut self, secs: u64, tx: &Sender<Msg>) {
        let secs = interval(secs);
        if secs == self.interval {
            return;
        }
        self.interval = secs;
        self.generation += 1;
        let names: Vec<String> = std::mem::take(&mut self.feeds)
            .iter()
            .map(|f| f.machine.clone())
            .collect();
        for m in names {
            match Feed::spawn(tx.clone(), m.clone(), self.interval, self.generation) {
                Ok(feed) => {
                    self.feeds.push(feed);
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

    pub fn receive(&mut self, msg: Msg) {
        match msg {
            Msg::State {
                generation,
                machine,
                status,
            } if generation == self.generation => {
                let now = Instant::now();
                let v = self.views.entry(machine).or_default();
                v.status = Some(status);
                v.seen_at = Some(now);
                v.dead = None;
                self.busy = None;
                self.close_round(now);
            }
            Msg::Trouble {
                generation,
                machine,
                text,
            } if generation == self.generation => {
                self.views.entry(machine).or_default().trouble = Some(text);
            }
            Msg::Ended {
                generation,
                machine,
                why,
            } if generation == self.generation => {
                self.views.entry(machine).or_default().dead = Some(why);
                self.close_round(Instant::now());
            }
            Msg::State { .. } | Msg::Trouble { .. } | Msg::Ended { .. } => {}
            Msg::Action { title, body } => {
                self.busy = None;
                self.overlay = Some(Overlay::new(title, body));
            }
        }
    }

    fn close_round(&mut self, now: Instant) {
        if self.round_over() {
            self.refreshed = Some(now);
            self.round_from = now;
        }
    }

    /// Every feed that can still report has reported since the round began. The header dates the
    /// screen by that, because it is the one moment all of it was current: the newest feed resets
    /// the number several times an interval with several machines, and the oldest walks up and
    /// down as they drift apart. A feed that has stopped reporting holds the round open, which is
    /// the staleness worth seeing.
    fn round_over(&self) -> bool {
        self.views
            .values()
            // After the round began, not at the moment it did: the feed whose report ended the
            // last round is the one that starts this one, and counting it twice leaves the round
            // needing only the others, which ends it early and by a different amount each time.
            .all(|v| v.dead.is_some() || v.seen_at.is_some_and(|t| t > self.round_from))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn view(seen: Option<Instant>, dead: bool) -> View {
        View {
            status: None,
            seen_at: seen,
            trouble: None,
            dead: dead.then(|| "gone".to_string()),
        }
    }

    #[test]
    fn a_round_is_over_when_every_feed_that_can_report_has() {
        let mut app = App::new(5);
        let now = Instant::now();
        app.views.insert("a".into(), view(Some(now), false));
        app.views.insert(
            "b".into(),
            view(Some(app.round_from - Duration::from_secs(1)), false),
        );
        assert!(
            !app.round_over(),
            "b has not reported since the round began"
        );
        app.views.insert("b".into(), view(Some(now), false));
        assert!(app.round_over());
        app.views
            .insert("c".into(), view(Some(app.round_from), false));
        assert!(
            !app.round_over(),
            "the report that ended the last round does not count in this one"
        );
        app.views.insert("c".into(), view(None, false));
        assert!(
            !app.round_over(),
            "a feed yet to say anything holds the round open"
        );
        app.views.insert("c".into(), view(None, true));
        assert!(app.round_over(), "one that has ended cannot report at all");
    }
}
