//! Drawing one frame: the header, the jobs table, the pane about the selected job, the footer,
//! and whatever is open over them.

use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap},
};

use crate::{
    app::{App, Overlay, PendingKill},
    item::Item,
    text::{agent_hue, cores, dur, fit},
};

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

/// Percent of the screen an overlay takes, across and down.
const OVERLAY_SIZE: (u16, u16) = (86, 84);
const CONFIRM_SIZE: (u16, u16) = (56, 22);

/// A feed this many intervals late, plus some slack, is called out as behind.
const LATE_INTERVALS: u64 = 3;
const LATE_SLACK_SECS: u64 = 2;

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
    let mut ages: Vec<u64> = Vec::new();
    for (name, v) in &app.views {
        if !head.is_empty() {
            head.push(Span::styled("   ", DIM));
        }
        if multi {
            head.push(Span::styled(format!("{name} "), DIM));
        }
        match (&v.dead, &v.status) {
            (Some(d), _) => head.push(Span::styled(
                if multi {
                    "down".to_string()
                } else {
                    format!("feed down: {d}")
                },
                Style::new().fg(Color::Red),
            )),
            (None, Some(s)) => {
                let (text, style) = state_style(&s.state);
                head.push(Span::styled(
                    if multi { text } else { format!("dibs: {text}") },
                    style,
                ));
                if !s.queue.is_empty() {
                    head.push(Span::styled(format!(" ({} queued)", s.queue.len()), DIM));
                }
            }
            (None, None) => head.push(Span::styled("connecting…", DIM)),
        }
        if let Some(t) = v.seen_at {
            let age = t.elapsed().as_secs();
            ages.push(age);
            if multi && behind(age, app.interval) {
                head.push(Span::styled(
                    format!(" {age}s behind"),
                    Style::new().fg(Color::Yellow),
                ));
            }
        }
    }
    if let Some(t) = app.refreshed {
        let age = t.elapsed().as_secs();
        let late = ages.iter().any(|a| behind(*a, app.interval));
        head.push(Span::styled(
            format!("   updated {age}s ago, every {}s", app.interval),
            if late {
                Style::new().fg(Color::Yellow)
            } else {
                DIM
            },
        ));
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
    let mut trows: Vec<Row> = rows
        .iter()
        .enumerate()
        .skip(app.top)
        .take(body_h.max(1))
        .map(|(i, it)| job_row(it, i == app.sel, multi, any_device))
        .collect();
    if trows.is_empty() {
        trows.push(Row::new(vec![Span::styled(
            "  nothing holding it, nothing queued".to_string(),
            Style::new().fg(Color::Green),
        )]));
    }

    let mut columns = vec![("", Constraint::Length(1))];
    if multi {
        columns.push(("MACHINE", Constraint::Length(MACHINE_WIDTH as u16 + 1)));
    }
    columns.extend([
        ("WHAT", Constraint::Length(9)),
        ("MODE", Constraint::Length(6)),
        ("LABEL", Constraint::Length(LABEL_WIDTH as u16)),
    ]);
    if any_device {
        columns.push(("DEVICE", Constraint::Length(DEVICE_WIDTH as u16 + 1)));
    }
    columns.extend([
        ("AGENT", Constraint::Length(AGENT_WIDTH as u16)),
        ("TIME", Constraint::Length(7)),
        ("CORES", Constraint::Length(7)),
        ("NOTE", Constraint::Min(24)),
    ]);
    let (heads, widths): (Vec<&str>, Vec<Constraint>) = columns.into_iter().unzip();

    let table = Table::new(trows, widths)
        .header(Row::new(heads).style(DIM.add_modifier(Modifier::BOLD)))
        .block(Block::default().borders(Borders::ALL).title(" jobs "));
    f.render_widget(table, area);
}

fn job_row(it: &Item, selected: bool, multi: bool, any_device: bool) -> Row<'static> {
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
    if multi {
        cells.push(Span::styled(
            fit(&it.machine, MACHINE_WIDTH),
            base.patch(DIM),
        ));
    }
    cells.extend([
        Span::styled(it.slot.clone(), slot_style),
        Span::styled(it.mode.clone(), mode_style(&it.mode, base)),
        Span::styled(fit(&it.label, LABEL_WIDTH), base.fg(hue)),
    ]);
    if any_device {
        // An unpinned job among pinned ones is the thing worth seeing, so it reads as a
        // dash rather than as blank space.
        cells.push(match &it.device {
            Some(d) => Span::styled(fit(d, DEVICE_WIDTH), base.fg(Color::Magenta)),
            None => Span::styled(fit("-", DEVICE_WIDTH), base.patch(DIM)),
        });
    }
    cells.extend([
        Span::styled(fit(&it.agent, AGENT_WIDTH), base.fg(hue)),
        Span::styled(dur(it.time), base.add_modifier(Modifier::BOLD)),
        Span::styled(
            it.rate.map(cores).unwrap_or_else(|| "-".into()),
            base.patch(DIM),
        ),
        Span::styled(
            it.note.clone(),
            if it.alarm {
                base.fg(Color::Yellow)
            } else {
                base.patch(DIM)
            },
        ),
    ]);
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
            .map(|e| format!("~{}", dur(e)))
            .unwrap_or_else(|| none.into())
    };
    if it.holding {
        let cpu = dur(it.cpu.unwrap_or(0));
        lines.push(Line::from(vec![
            Span::styled("running ", DIM),
            Span::raw(dur(it.time)),
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
            Span::raw(dur(it.time)),
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
            Span::raw(b.id.clone()),
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

/// With no job selected, the pane says what the feeds have complained about.
fn troubles(app: &App) -> Paragraph<'static> {
    let text = app
        .views
        .values()
        .filter_map(|v| v.trouble.clone())
        .collect::<Vec<_>>()
        .join("\n");
    Paragraph::new(Span::styled(text, Style::new().fg(Color::Yellow)))
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
fn mode_style(mode: &str, base: Style) -> Style {
    base.fg(if mode == "bench" {
        Color::Red
    } else {
        Color::Yellow
    })
    .add_modifier(Modifier::BOLD)
}

fn state_style(state: &str) -> (String, Style) {
    let bold = |c: Color| Style::new().fg(c).add_modifier(Modifier::BOLD);
    match state {
        "bench" => ("BUSY, benchmark in progress".into(), bold(Color::Red)),
        "shared" => ("in use, shared".into(), bold(Color::Yellow)),
        "idle" => ("idle".into(), bold(Color::Green)),
        "busy" => ("in use".into(), bold(Color::Yellow)),
        "orphan" => ("LOCKED BY AN ORPHAN".into(), bold(Color::Red)),
        other => (other.into(), Style::new()),
    }
}

fn centred(area: Rect, (pct_x, pct_y): (u16, u16)) -> Rect {
    let h = area.height * pct_y / 100;
    let w = area.width * pct_x / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn behind(age: u64, interval: u64) -> bool {
    age > interval * LATE_INTERVALS + LATE_SLACK_SECS
}
