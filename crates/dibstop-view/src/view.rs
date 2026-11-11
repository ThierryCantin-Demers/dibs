//! dibstop drawn with egui, for a window or a web page. The caller passes every machine's
//! status each frame and runs the action `ui` returns, then hands back what it printed with
//! `answered`. The keys are dibstop's, so a note that says "Stop it with K" holds here too.

use dibs_format::{Label, Mode, Span, status::Status};
use egui::{Color32, Key, RichText, Ui};

use crate::action::Action;
use crate::item::{Item, Progress as _};
use crate::text::{Tone, agent_hue, cores, plain, state_words};

/// One machine, as the caller last heard of it.
pub struct Machine {
    pub name: String,
    /// None until its first status arrives.
    pub status: Option<Status>,
    /// What its feed last said that was not a status, such as why it cannot reach it.
    pub trouble: Option<String>,
}

#[derive(Default)]
pub struct View {
    /// By machine and pid, so the selection follows its job as the rows reorder.
    selected: Option<(String, u32)>,
    confirm: Option<Kill>,
    /// The title and body of what an action printed.
    shown: Option<(String, String)>,
    /// What the action the caller is running is doing.
    busy: Option<String>,
}

struct Kill {
    machine: String,
    pid: u32,
    label: Label,
}

impl View {
    /// Draws the machines, and returns an action the person asked for.
    pub fn ui(&mut self, ui: &mut Ui, machines: &[Machine]) -> Option<Action> {
        let rows: Vec<Item> = machines
            .iter()
            .filter_map(|machine| Some((machine, machine.status.as_ref()?)))
            .flat_map(|(machine, status)| Item::all(&machine.name, status))
            .collect();
        let at = self.selected.as_ref().and_then(|(machine, pid)| {
            rows.iter()
                .position(|it| it.machine == *machine && it.pid == *pid)
        });
        let mut asked = self.keys(ui, &rows, at, machines);
        let multi = machines.len() > 1;
        header(ui, machines, self.busy.as_deref());
        ui.separator();
        self.table(ui, &rows, at, multi);
        ui.separator();
        let current = at.and_then(|at| rows.get(at));
        match current {
            Some(it) => asked = asked.or(self.detail(ui, it)),
            None => troubles(ui, machines),
        }
        let machine = current
            .map(|it| it.machine.clone())
            .or_else(|| machines.first().map(|machine| machine.name.clone()));
        if let Some(machine) = machine {
            ui.horizontal(|ui| {
                ui.weak(&machine);
                if ui.button("L  log").clicked() {
                    asked = Some(Action::log(machine.clone()));
                }
                if ui.button("n  gpu").clicked() {
                    asked = Some(Action::gpu(machine.clone()));
                }
                if ui.button("r  refresh").clicked() {
                    asked = Some(Action::status(machine.clone()));
                }
            });
        }
        asked = asked.or(self.overlays(ui));
        if let Some(action) = &asked {
            self.busy = Some(action.doing.clone());
        }
        asked
    }

    /// What the last action printed, shown over the view until it is closed.
    pub fn answered(&mut self, title: String, body: String) {
        self.busy = None;
        self.shown = Some((title, body));
    }

    fn keys(
        &mut self,
        ui: &Ui,
        rows: &[Item],
        at: Option<usize>,
        machines: &[Machine],
    ) -> Option<Action> {
        // A row or a button the person clicked keeps the focus; only typing takes the keys.
        if !ui.is_enabled() || ui.ctx().text_edit_focused() {
            return None;
        }
        let (pressed, shift) = ui.input(|input| {
            let keys = [
                Key::J,
                Key::K,
                Key::ArrowDown,
                Key::ArrowUp,
                Key::P,
                Key::Enter,
                Key::O,
                Key::N,
                Key::L,
                Key::R,
                Key::Y,
                Key::Escape,
            ];
            let pressed = keys.into_iter().find(|key| input.key_pressed(*key));
            (pressed, input.modifiers.shift)
        });
        let key = pressed?;
        // Enter presses the focused button, as in any egui window.
        if key == Key::Enter && ui.ctx().egui_wants_keyboard_input() {
            return None;
        }
        if key == Key::Escape {
            self.confirm = None;
            self.shown = None;
            return None;
        }
        if let Some(kill) = self.confirm.take() {
            return (key == Key::Y).then(|| Action::kill(kill.machine, kill.pid, &kill.label));
        }
        if self.shown.is_some() {
            return None;
        }
        let it = at.and_then(|at| rows.get(at));
        let machine = || {
            it.map(|it| it.machine.clone())
                .or_else(|| machines.first().map(|machine| machine.name.clone()))
        };
        match key {
            Key::K if shift => {
                self.confirm = it.map(|it| Kill {
                    machine: it.machine.clone(),
                    pid: it.pid,
                    label: it.label.clone(),
                });
                None
            }
            Key::J | Key::ArrowDown | Key::K | Key::ArrowUp => {
                let down = matches!(key, Key::J | Key::ArrowDown);
                let next = match at {
                    Some(at) if down => (at + 1).min(rows.len().saturating_sub(1)),
                    Some(at) => at.saturating_sub(1),
                    None => 0,
                };
                self.selected = rows.get(next).map(|it| (it.machine.clone(), it.pid));
                None
            }
            Key::P | Key::Enter => it.map(Action::process_tree),
            Key::O => it.filter(|it| it.output.is_some()).map(Action::output),
            Key::N => machine().map(Action::gpu),
            Key::L => machine().map(Action::log),
            Key::R => machine().map(Action::status),
            _ => None,
        }
    }

    /// The jobs as cards that fit the width they are given, since a page shares its width
    /// with other views where a terminal gives dibstop all of its own.
    fn table(&mut self, ui: &mut Ui, rows: &[Item], at: Option<usize>, multi: bool) {
        if rows.is_empty() {
            ui.colored_label(green(ui), "nothing holding it, nothing queued");
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("dibstop-jobs")
            .max_height(ui.available_height() * 0.6)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for running in [true, false] {
                    let jobs: Vec<(usize, &Item)> = rows
                        .iter()
                        .enumerate()
                        .filter(|(_, it)| it.holding == running)
                        .collect();
                    if jobs.is_empty() {
                        continue;
                    }
                    let heading = match running {
                        true => RichText::new(format!("Running now ({})", jobs.len()))
                            .color(green(ui))
                            .strong(),
                        false => RichText::new(format!("Waiting ({})", jobs.len())).weak(),
                    };
                    ui.label(heading);
                    for (i, it) in jobs {
                        if self.card(ui, it, at == Some(i), multi) {
                            self.selected = Some((it.machine.clone(), it.pid));
                        }
                        ui.add_space(4.0);
                    }
                    ui.add_space(6.0);
                }
            });
    }

    /// One job: what it is and how long it has run, its label, who and where, and its note.
    /// Returns whether it was clicked.
    fn card(&mut self, ui: &mut Ui, it: &Item, selected: bool, multi: bool) -> bool {
        let hue = xterm(agent_hue(&it.agent));
        let mode = match it.mode {
            Mode::Bench => ui.visuals().error_fg_color,
            _ => ui.visuals().warn_fg_color,
        };
        let fill = match (selected, it.holding) {
            (true, _) => ui.visuals().selection.bg_fill.gamma_multiply(0.5),
            (false, true) => green(ui).gamma_multiply(0.12),
            (false, false) => ui.visuals().faint_bg_color,
        };
        let card = egui::Frame::new()
            .fill(fill)
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let frozen = it.holding && it.slot != "HOLDING";
                    let slot = match (it.holding, frozen || it.alarm) {
                        (true, true) => RichText::new(it.slot.to_lowercase())
                            .color(ui.visuals().warn_fg_color)
                            .strong(),
                        (true, false) => RichText::new("running").color(green(ui)).strong(),
                        (false, _) => RichText::new(it.slot.replace("queued ", "#") + " in queue")
                            .color(Color32::from_rgb(80, 180, 200)),
                    };
                    ui.label(slot);
                    ui.label(RichText::new(it.mode.to_string()).color(mode).strong());
                    if multi {
                        ui.weak(&it.machine);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let time = Span(it.time).to_string();
                        match it.holding {
                            true => ui.label(RichText::new(format!("for {time}")).strong()),
                            false => ui.weak(format!("waited {time}")),
                        };
                    });
                });
                ui.add(egui::Label::new(RichText::new(it.label.as_str()).color(hue)).truncate());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&it.agent).color(hue));
                    if let Some(device) = &it.device {
                        ui.label(
                            RichText::new(device.as_str()).color(Color32::from_rgb(200, 110, 200)),
                        );
                    }
                    if let Some(rate) = it.rate {
                        ui.weak(cores(rate));
                    }
                    if let Some(batch) = &it.batch {
                        ui.label(batch.progress());
                    }
                    // A queued job's note already says when it starts.
                    if let Some(eta) = it.eta.filter(|_| it.holding) {
                        ui.weak(format!("job ~{} left", Span(eta)));
                    }
                });
                if !it.note.is_empty() {
                    match it.alarm {
                        true => ui.colored_label(ui.visuals().warn_fg_color, &it.note),
                        false => ui.weak(&it.note),
                    };
                }
            });
        if it.holding {
            let rect = card.response.rect;
            let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
            ui.painter().rect_filled(stripe, 2.0, mode);
        }
        card.response.interact(egui::Sense::click()).clicked()
    }

    fn detail(&mut self, ui: &mut Ui, it: &Item) -> Option<Action> {
        let mut asked = None;
        ui.horizontal_wrapped(|ui| {
            ui.weak("from");
            ui.label(RichText::new(&it.agent).color(xterm(agent_hue(&it.agent))));
            ui.weak(format!("pid {} on {}", it.pid, it.machine));
        });
        ui.label(RichText::new(&it.cmd).monospace());
        let eta = |none: &str| {
            it.eta
                .map(|eta| format!("~{}", Span(eta)))
                .unwrap_or_else(|| none.into())
        };
        ui.horizontal_wrapped(|ui| match it.holding {
            true => {
                let cpu = Span(it.cpu.unwrap_or(0));
                ui.weak("running");
                ui.label(Span(it.time).to_string());
                ui.weak("cpu");
                ui.label(match it.rate {
                    Some(rate) => format!("{cpu} across all cores, {} right now", cores(rate)),
                    None => format!("{cpu} across all cores"),
                });
                ui.weak("left");
                ui.label(eta("unknown"));
            }
            false => {
                ui.weak("waiting");
                ui.label(Span(it.time).to_string());
                ui.weak("starts in");
                ui.label(eta("unknown"));
            }
        });
        if let Some(out) = &it.output {
            ui.horizontal_wrapped(|ui| {
                ui.weak("writing");
                ui.label(RichText::new(out).monospace());
            });
        }
        if let Some(batch) = &it.batch {
            ui.horizontal_wrapped(|ui| {
                ui.weak("batch");
                ui.label(batch.id.to_string());
                ui.weak(format!("step {} of {}:", batch.k, batch.n));
                ui.label(&batch.step);
                if let Some(within) = batch.within {
                    ui.weak(format!(
                        "job {} of {} in this step",
                        within.job, within.jobs
                    ));
                }
                if let Some(left) = batch.left_text() {
                    ui.weak("left here");
                    ui.label(left);
                }
            });
            if !batch.next.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.weak("then here");
                    ui.label(&batch.next);
                });
            }
        }
        match it.alarm {
            true => ui.colored_label(ui.visuals().warn_fg_color, format!("\u{26a0} {}", it.long)),
            false => ui.weak(&it.long),
        };
        ui.horizontal(|ui| {
            if ui.button("p  process tree").clicked() {
                asked = Some(Action::process_tree(it));
            }
            if it.output.is_some() && ui.button("o  output").clicked() {
                asked = Some(Action::output(it));
            }
            let kill = RichText::new("K  kill").color(ui.visuals().error_fg_color);
            if ui.button(kill).clicked() {
                self.confirm = Some(Kill {
                    machine: it.machine.clone(),
                    pid: it.pid,
                    label: it.label.clone(),
                });
            }
        });
        asked
    }

    fn overlays(&mut self, ui: &Ui) -> Option<Action> {
        let mut asked = None;
        // A modal is an area of its own, which does not take the view's disabled state.
        let enabled = ui.is_enabled();
        if let Some(kill) = &self.confirm {
            let mut answer = None;
            egui::Modal::new(egui::Id::new("dibstop-confirm")).show(ui.ctx(), |ui| {
                if !enabled {
                    ui.disable();
                }
                ui.heading("Stop this job?");
                ui.label(format!(
                    "Kill {} ({}) on {}, whoever started it. It runs dibs --kill {} --anyone.",
                    kill.pid, kill.label, kill.machine, kill.pid
                ));
                ui.horizontal(|ui| {
                    if ui.button("y  kill it").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Esc  leave it").clicked() {
                        answer = Some(false);
                    }
                });
            });
            if let Some(yes) = answer {
                let kill = self.confirm.take().expect("shown above");
                asked = yes.then(|| Action::kill(kill.machine, kill.pid, &kill.label));
            }
        }
        if let Some((title, body)) = &self.shown {
            let mut close = false;
            egui::Modal::new(egui::Id::new("dibstop-shown")).show(ui.ctx(), |ui| {
                if !enabled {
                    ui.disable();
                }
                ui.set_max_width(ui.ctx().content_rect().width() * 0.8);
                ui.heading(title);
                egui::ScrollArea::both()
                    .max_height(ui.ctx().content_rect().height() * 0.7)
                    .show(ui, |ui| ui.label(RichText::new(body).monospace()));
                close = ui.button("Esc  close").clicked();
            });
            if close {
                self.shown = None;
            }
        }
        asked
    }
}

fn header(ui: &mut Ui, machines: &[Machine], busy: Option<&str>) {
    ui.horizontal_wrapped(|ui| {
        for machine in machines {
            ui.add_space(8.0);
            if machines.len() > 1 {
                ui.weak(&machine.name);
            }
            match (&machine.status, &machine.trouble) {
                (Some(status), _) => {
                    let (words, tone) = state_words(status.state);
                    let colour = match tone {
                        Tone::Free => green(ui),
                        Tone::InUse => ui.visuals().warn_fg_color,
                        Tone::Alarm => ui.visuals().error_fg_color,
                    };
                    ui.label(RichText::new(words).color(colour).strong());
                    if !status.queue.is_empty() {
                        ui.weak(format!("({} queued)", status.queue.len()));
                    }
                    if let Some(closed) = &status.closed {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("closed by {}", closed.by),
                        );
                    }
                }
                (None, Some(_)) => {
                    ui.colored_label(ui.visuals().error_fg_color, "down");
                }
                (None, None) => {
                    ui.weak("connecting…");
                }
            }
        }
        if let Some(busy) = busy {
            ui.add_space(8.0);
            ui.colored_label(Color32::from_rgb(80, 180, 200), format!("· {busy}…"));
        }
    });
}

fn troubles(ui: &mut Ui, machines: &[Machine]) {
    let mut any = false;
    for machine in machines {
        let Some(trouble) = &machine.trouble else {
            continue;
        };
        any = true;
        ui.horizontal_wrapped(|ui| {
            ui.weak(&machine.name);
            ui.colored_label(ui.visuals().warn_fg_color, plain(trouble));
        });
    }
    if !any {
        ui.weak("Select a job to see what it is doing.");
    }
}

fn green(ui: &Ui) -> Color32 {
    match ui.visuals().dark_mode {
        true => Color32::from_rgb(90, 190, 110),
        false => Color32::from_rgb(40, 140, 60),
    }
}

/// An xterm 256-colour index as the colour a terminal shows it in.
fn xterm(index: u8) -> Color32 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        16..=231 => {
            let cube = index - 16;
            Color32::from_rgb(
                LEVELS[usize::from(cube / 36)],
                LEVELS[usize::from(cube / 6 % 6)],
                LEVELS[usize::from(cube % 6)],
            )
        }
        232.. => {
            let grey = 8 + (index - 232) * 10;
            Color32::from_rgb(grey, grey, grey)
        }
        _ => Color32::GRAY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agents_colour_is_the_one_its_terminal_shows() {
        assert_eq!(xterm(33), Color32::from_rgb(0, 135, 255));
        assert_eq!(xterm(205), Color32::from_rgb(255, 95, 175));
    }
}
