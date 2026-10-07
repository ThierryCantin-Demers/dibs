//! Drawing one frame: the header, the jobs table, the pane about the selected job, the footer,
//! and whatever is open over them.

use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap},
};

use crate::{
    app::{App, Overlay, PendingKill},
    item::{Item, Progress as _},
    text::{agent_hue, cores, fit, plain},
};
use dibs_format::{Mode, Span as DibsSpan, status::LockState};

const DIM: Style = Style::new().fg(Color::DarkGray);
const SELECTED_BG: Color = Color::Indexed(236);

const TABLE_MIN_HEIGHT: u16 = 4;
const PANE_HEIGHT: u16 = 8;
/// A batch step takes two more lines: the step, and what comes after it.
const PANE_HEIGHT_IN_BATCH: u16 = 10;

const MACHINE_WIDTH: usize = 15;
const LABEL_WIDTH: usize = 18;
const DEVICE_WIDTH: usize = 16;
const AGENT_WIDTH: usize = 20;
const WHAT_WIDTH: usize = 9;
const MODE_WIDTH: usize = 6;
const TIME_WIDTH: usize = 7;
const CORES_WIDTH: usize = 7;
const NOTE_MIN_WIDTH: usize = 24;

const OVERLAY_SIZE: Share = Share {
    width: 86,
    height: 84,
};
const CONFIRM_SIZE: Share = Share {
    width: 56,
    height: 22,
};

/// Percent of the screen an overlay takes, across and down.
#[derive(Clone, Copy)]
struct Share {
    width: u16,
    height: u16,
}

/// A table column: its heading, and its width in cells.
struct Column {
    heading: &'static str,
    width: usize,
}

/// A machine's lock state as the header words and colours it.
struct Worded {
    text: &'static str,
    style: Style,
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let rows = app.rows();
    let in_batch = rows.get(app.sel).is_some_and(|it| it.batch.is_some());
    let [head, jobs, pane, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(TABLE_MIN_HEIGHT),
        Constraint::Length(if in_batch {
            PANE_HEIGHT_IN_BATCH
        } else {
            PANE_HEIGHT
        }),
        Constraint::Length(1),
    ])
    .areas(f.area());

    f.render_widget(Paragraph::new(header(app)), head);
    jobs_table(f, app, &rows, jobs);
    let selected = match rows.get(app.sel) {
        Some(it) => job_detail(it),
        None => troubles(app),
    };
    f.render_widget(
        selected.block(Block::default().borders(Borders::ALL).title(" selected ")),
        pane,
    );
    f.render_widget(footer(app), foot);

    if let Some(o) = &app.overlay {
        overlay(f, o);
    }
    if let Some(kill) = &app.confirm {
        confirm(f, kill);
    }
}

/// One group per machine rather than a single verdict: "the feed is down" and "it is idle" are
/// different answers, and a summary across machines can only give one of them.
fn header(app: &App) -> Line<'static> {
    let mut head: Vec<Span> = Vec::new();
    let multi = app.multi();
    let mut late = false;
    for (name, v) in &app.views {
        if !head.is_empty() {
            head.push(Span::styled("   ", DIM));
        }
        if multi {
            head.push(Span::styled(format!("{name} "), DIM));
        }
        match (&v.dead, &v.status) {
            (Some(d), _) => head.push(Span::styled(
                match (multi, v.refusal()) {
                    (_, Some(why)) => why.to_string(),
                    (true, None) => "down".to_string(),
                    (false, None) => format!("feed down: {d}"),
                },
                Style::new().fg(Color::Red),
            )),
            (None, Some(s)) => {
                let Worded { text, style } = Worded::of(s.state);
                head.push(Span::styled(
                    if multi {
                        text.to_string()
                    } else {
                        format!("dibs: {text}")
                    },
                    style,
                ));
                if !s.queue.is_empty() {
                    head.push(Span::styled(format!(" ({} queued)", s.queue.len()), DIM));
                }
            }
            (None, None) => head.push(Span::styled("connecting…", DIM)),
        }
        if let Some(t) = v.seen_at.filter(|t| app.behind(*t)) {
            late = true;
            if multi {
                head.push(Span::styled(
                    format!(" {}s behind", t.elapsed().as_secs()),
                    Style::new().fg(Color::Yellow),
                ));
            }
        }
    }
    let every = format!("every {}s", app.interval);
    match app.refreshed {
        Some(t) => {
            let age = t.elapsed().as_secs();
            head.push(Span::styled(
                format!("   updated {age}s ago, {every}"),
                if late {
                    Style::new().fg(Color::Yellow)
                } else {
                    DIM
                },
            ));
        }
        None => head.push(Span::styled(format!("   {every}"), DIM)),
    }
    if let Some(b) = &app.busy {
        head.push(Span::styled(
            format!("  · {b}…"),
            Style::new().fg(Color::Cyan),
        ));
    }
    Line::from(head)
}

fn jobs_table(f: &mut Frame, app: &mut App, rows: &[Item], area: Rect) {
    // The border, then the header row.
    let body_h = area.height.saturating_sub(3) as usize;
    if app.sel >= rows.len() {
        app.sel = rows.len().saturating_sub(1);
    }
    if app.sel < app.top {
        app.top = app.sel;
    } else if body_h > 0 && app.sel >= app.top + body_h {
        app.top = app.sel + 1 - body_h;
    }
    app.table_top = area.y + 2;
    app.table_rows = body_h as u16;

    let multi = app.multi();
    let any_device = rows.iter().any(|it| it.device.is_some());
    let w = Widths::new(
        rows,
        area.width.saturating_sub(2) as usize,
        multi,
        any_device,
    );
    let mut trows: Vec<Row> = rows
        .iter()
        .enumerate()
        .skip(app.top)
        .take(body_h.max(1))
        .map(|(i, it)| job_row(it, i == app.sel, &w))
        .collect();
    if trows.is_empty() {
        trows.push(Row::new(vec![Span::styled(
            "  nothing holding it, nothing queued".to_string(),
            Style::new().fg(Color::Green),
        )]));
    }

    let columns = w.columns();
    let last = columns.len() - 1;
    let (heads, widths): (Vec<&str>, Vec<Constraint>) = columns
        .into_iter()
        .enumerate()
        .map(|(i, column)| match i == last {
            true => (column.heading, Constraint::Min(column.width as u16)),
            false => (column.heading, Constraint::Length(column.width as u16)),
        })
        .unzip();

    let table = Table::new(trows, widths)
        .header(Row::new(heads).style(DIM.add_modifier(Modifier::BOLD)))
        .block(Block::default().borders(Borders::ALL).title(" jobs "));
    f.render_widget(table, area);
}

/// The columns whose text varies: never narrower than their defaults, and wider toward their
/// longest entry as far as the screen has room, so a long label, agent or note is not cut beside
/// space nothing uses.
struct Widths {
    multi: bool,
    any_device: bool,
    any_step: bool,
    machine: usize,
    label: usize,
    device: usize,
    agent: usize,
    step: usize,
    note: usize,
}

impl Widths {
    fn new(rows: &[Item], inner: usize, multi: bool, any_device: bool) -> Widths {
        let longest =
            |len: &dyn Fn(&Item) -> Option<usize>| rows.iter().filter_map(len).max().unwrap_or(0);
        let count = |s: &str| s.chars().count();
        let step = longest(&|it| it.batch.as_ref().map(|b| count(&b.progress())));
        let mut w = Widths {
            multi,
            any_device,
            any_step: step > 0,
            machine: MACHINE_WIDTH,
            label: LABEL_WIDTH,
            device: DEVICE_WIDTH,
            agent: AGENT_WIDTH,
            step: step.max(count("STEP")),
            note: NOTE_MIN_WIDTH,
        };
        let columns = w.columns();
        let taken = columns.iter().map(|c| c.width).sum::<usize>() + columns.len() - 1;
        let mut spare = inner.saturating_sub(taken);
        let mut grow = [
            (
                multi,
                &mut w.machine,
                longest(&|it| Some(count(&it.machine))),
            ),
            (
                true,
                &mut w.label,
                longest(&|it| Some(count(it.label.as_str()))),
            ),
            (
                any_device,
                &mut w.device,
                longest(&|it| it.device.as_ref().map(|d| count(d.as_str()))),
            ),
            (true, &mut w.agent, longest(&|it| Some(count(&it.agent)))),
            (true, &mut w.note, longest(&|it| Some(count(&it.note)))),
        ];
        // One character at a time, round the columns, so a long label, agent and note share what
        // room there is rather than the first taking all of it.
        while spare > 0 {
            let before = spare;
            for (on, have, want) in grow.iter_mut() {
                if spare > 0 && *on && **have < *want {
                    **have += 1;
                    spare -= 1;
                }
            }
            if spare == before {
                break;
            }
        }
        w
    }

    /// Each column shown, its heading and its width. The machine and device columns carry a
    /// space of their own past what they print.
    fn columns(&self) -> Vec<Column> {
        let mut c = vec![Column::new("", 1)];
        if self.multi {
            c.push(Column::new("MACHINE", self.machine + 1));
        }
        c.extend([
            Column::new("WHAT", WHAT_WIDTH),
            Column::new("MODE", MODE_WIDTH),
            Column::new("LABEL", self.label),
        ]);
        if self.any_device {
            c.push(Column::new("DEVICE", self.device + 1));
        }
        c.extend([
            Column::new("AGENT", self.agent),
            Column::new("TIME", TIME_WIDTH),
            Column::new("CORES", CORES_WIDTH),
        ]);
        if self.any_step {
            c.push(Column::new("STEP", self.step));
        }
        c.push(Column::new("NOTE", self.note));
        c
    }
}

fn job_row(it: &Item, selected: bool, w: &Widths) -> Row<'static> {
    let hue = agent_hue(&it.agent);
    let base = match selected {
        true => Style::new().bg(SELECTED_BG).add_modifier(Modifier::BOLD),
        false => Style::new(),
    };
    let slot_style = match it.holding {
        true => base.fg(if it.alarm {
            Color::Yellow
        } else {
            Color::White
        }),
        false => base.fg(Color::Cyan),
    };
    let mut cells = vec![Span::styled(if selected { "▸" } else { " " }, base.fg(hue))];
    if w.multi {
        cells.push(Span::styled(fit(&it.machine, w.machine), base.patch(DIM)));
    }
    cells.extend([
        Span::styled(it.slot.clone(), slot_style),
        Span::styled(it.mode.to_string(), mode_style(it.mode, base)),
        Span::styled(fit(it.label.as_str(), w.label), base.fg(hue)),
    ]);
    if w.any_device {
        // An unpinned job among pinned ones is the thing worth seeing, so it reads as a
        // dash rather than as blank space.
        cells.push(match &it.device {
            Some(d) => Span::styled(fit(d.as_str(), w.device), base.fg(Color::Magenta)),
            None => Span::styled(fit("-", w.device), base.patch(DIM)),
        });
    }
    cells.extend([
        Span::styled(fit(&it.agent, w.agent), base.fg(hue)),
        Span::styled(
            DibsSpan(it.time).to_string(),
            base.add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            it.rate.map(cores).unwrap_or_else(|| "-".into()),
            base.patch(DIM),
        ),
    ]);
    if w.any_step {
        cells.push(match &it.batch {
            Some(b) => Span::styled(b.progress(), base),
            None => Span::styled("-", base.patch(DIM)),
        });
    }
    cells.extend([Span::styled(
        it.note.clone(),
        if it.alarm {
            base.fg(Color::Yellow)
        } else {
            base.patch(DIM)
        },
    )]);
    Row::new(cells).style(base)
}

fn job_detail(it: &Item) -> Paragraph<'static> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("from  ", DIM),
            Span::styled(it.agent.clone(), Style::new().fg(agent_hue(&it.agent))),
            Span::styled(format!("   pid {}", it.pid), DIM),
        ]),
        Line::from(Span::raw(it.cmd.clone())),
    ];
    let eta = |none: &str| {
        it.eta
            .map(|e| format!("~{}", DibsSpan(e)))
            .unwrap_or_else(|| none.into())
    };
    if it.holding {
        let cpu = DibsSpan(it.cpu.unwrap_or(0));
        lines.push(Line::from(vec![
            Span::styled("running ", DIM),
            Span::raw(DibsSpan(it.time).to_string()),
            Span::styled("   cpu ", DIM),
            Span::raw(match it.rate {
                Some(r) => format!("{cpu} across all cores, {} right now", cores(r)),
                None => format!("{cpu} across all cores"),
            }),
            Span::styled("   left ", DIM),
            Span::raw(eta("unknown")),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("waiting ", DIM),
            Span::raw(DibsSpan(it.time).to_string()),
            Span::styled("   starts in ", DIM),
            Span::raw(eta("no telling")),
        ]));
    }
    // Where its output is going, so `o` is discovered by looking rather than by already
    // knowing. A job that redirected nowhere gets nothing here, because a key offered for a job
    // it cannot read is worse than no offer.
    if let Some(out) = &it.output {
        lines.push(Line::from(vec![
            Span::styled("writing ", DIM),
            Span::raw(out.clone()),
            Span::styled("   press o", DIM),
        ]));
    }
    if let Some(b) = &it.batch {
        let mut first = vec![
            Span::styled("batch ", DIM),
            Span::raw(b.id.to_string()),
            Span::styled(format!("   step {} of {}: ", b.k, b.n), DIM),
            Span::raw(b.step.clone()),
        ];
        if let Some(l) = b.left_text() {
            first.extend([Span::styled("   left here ", DIM), Span::raw(l)]);
        }
        lines.push(Line::from(first));
        let mut then = Vec::new();
        if !b.next.is_empty() {
            then.extend([Span::styled("then here ", DIM), Span::raw(b.next.clone())]);
        }
        if !b.far.is_empty() {
            then.push(Span::styled(
                if then.is_empty() {
                    "elsewhere "
                } else {
                    "   elsewhere "
                },
                DIM,
            ));
            then.push(Span::raw(b.far.clone()));
        }
        if !then.is_empty() {
            lines.push(Line::from(then));
        }
    }
    lines.push(match it.alarm {
        true => Line::from(Span::styled(
            format!("\u{26a0} {}", it.long),
            Style::new().fg(Color::Yellow),
        )),
        false => Line::from(Span::styled(it.long.clone(), DIM)),
    });
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

/// With no job selected, the pane says what the feeds have complained about, each under the
/// machine it came from.
fn troubles(app: &App) -> Paragraph<'static> {
    let width = app
        .views
        .keys()
        .map(|m| m.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = app
        .views
        .iter()
        .filter_map(|(m, v)| v.trouble.as_ref().map(|t| (m, t)))
        .flat_map(|(m, t)| {
            t.lines().enumerate().map(move |(i, l)| {
                let name = if i == 0 { m.as_str() } else { "" };
                Line::from(vec![
                    Span::styled(format!("{name:width$}  "), DIM),
                    Span::styled(plain(l), Style::new().fg(Color::Yellow)),
                ])
            })
        })
        .collect();
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

fn footer(app: &App) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        format!(
            "  j/k move   p tree   o output   n gpu   L log   K kill   +/- rate   m mouse:{}   ? keys   q quit",
            if app.mouse { "on" } else { "off" }
        ),
        DIM,
    ))
}

fn overlay(f: &mut Frame, o: &Overlay) {
    let area = centred(f.area(), OVERLAY_SIZE);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(o.body.clone()).scroll((o.scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", o.title))
                .title_bottom(" j/k scroll   esc close "),
        ),
        area,
    );
}

fn confirm(f: &mut Frame, kill: &PendingKill) {
    let area = centred(f.area(), CONFIRM_SIZE);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Stop {} (pid {})?", kill.label, kill.pid),
                Style::new().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  It is someone's work. y to send SIGTERM, anything else cancels.",
                DIM,
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

/// The same two colours the header uses for the machine's state, so a row reads the same
/// way as the line summarising it.
fn mode_style(mode: Mode, base: Style) -> Style {
    base.fg(if mode == Mode::Bench {
        Color::Red
    } else {
        Color::Yellow
    })
    .add_modifier(Modifier::BOLD)
}

impl Column {
    fn new(heading: &'static str, width: usize) -> Column {
        Column { heading, width }
    }
}

impl Worded {
    fn of(state: LockState) -> Worded {
        let bold = |c: Color| Style::new().fg(c).add_modifier(Modifier::BOLD);
        let (text, style) = match state {
            LockState::Bench => ("BUSY, benchmark in progress", bold(Color::Red)),
            LockState::Shared => ("in use, shared", bold(Color::Yellow)),
            LockState::Idle => ("idle", bold(Color::Green)),
            LockState::Busy => ("in use", bold(Color::Yellow)),
            LockState::Orphan => ("LOCKED BY AN ORPHAN", bold(Color::Red)),
        };
        Worded { text, style }
    }
}

fn centred(area: Rect, share: Share) -> Rect {
    let h = area.height * share.height / 100;
    let w = area.width * share.width / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibs_format::status::Status;

    #[test]
    fn a_long_label_agent_and_note_widen_as_far_as_the_screen_allows() {
        let label = "gemv-rows-with-a-long-label";
        let agent = "cubecl-cpu load_width detection";
        let s: Status = serde_json::from_str(&format!(
            r#"{{"t":0,"state":"shared","cores":1,"load":0,"caches":[],"clones":[],"holders":[{{"mode":"shared","pid":7,"label":"{label}","agent":"{agent}","device":"-","cmd":"c","started":0,"elapsed":3,"cpu":1,"est":10,"est_lo":8,"est_hi":12,"est_n":3,"est_scope":"this","est_other_values":true}}],"queue":[]}}"#
        ))
        .unwrap();
        let rows = Item::all("m", &s);
        let note = rows[0].note.chars().count();
        let wide = Widths::new(&rows, 200, false, false);
        assert_eq!(
            (wide.label, wide.agent, wide.note),
            (label.len(), agent.len(), note)
        );
        let narrow = Widths::new(&rows, 80, false, false);
        assert_eq!(
            (narrow.label, narrow.agent, narrow.note),
            (LABEL_WIDTH, AGENT_WIDTH, NOTE_MIN_WIDTH),
            "never narrower than the defaults"
        );
        let base: usize =
            narrow.columns().iter().map(|c| c.width).sum::<usize>() + narrow.columns().len() - 1;
        let some = Widths::new(&rows, base + 6, false, false);
        assert_eq!(
            (some.label, some.agent, some.note),
            (LABEL_WIDTH + 2, AGENT_WIDTH + 2, NOTE_MIN_WIDTH + 2),
            "the room is shared between them"
        );
        assert!(!some.any_step, "no step column without a batch");
    }
}
