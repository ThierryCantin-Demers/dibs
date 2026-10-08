//! A window on `dibs machines`: what each machine lacks against what it should have, probed in
//! this process through the dibs library.

use dibs::fleet::{self, FleetError};
use dibs_format::fleet::{Area, Overview, Report, Standing};
use eframe::egui::{self, Color32, RichText, Ui};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const GOOD: Color32 = Color32::from_rgb(90, 180, 100);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("dibs machines")
            .with_inner_size([1000.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "dibs-machines",
        options,
        Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
    )
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Machines,
    People,
}

/// A probe in flight, of every machine or of the one named.
struct Probe {
    only: Option<String>,
    done: mpsc::Receiver<Result<Overview, FleetError>>,
}

impl Probe {
    fn start(ctx: &egui::Context, only: Option<String>) -> Probe {
        let (tx, done) = mpsc::channel();
        let (ctx, name) = (ctx.clone(), only.clone());
        std::thread::spawn(move || {
            let _ = tx.send(fleet::survey(name.as_deref()));
            ctx.request_repaint();
        });
        Probe { only, done }
    }
}

struct App {
    overview: Overview,
    probing: Option<Probe>,
    probed_at: Option<Instant>,
    error: Option<FleetError>,
    selected: Option<String>,
    tab: Tab,
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        App {
            overview: Overview::default(),
            probing: Some(Probe::start(ctx, None)),
            probed_at: None,
            error: None,
            selected: None,
            tab: Tab::Machines,
        }
    }

    fn probe(&mut self, ctx: &egui::Context, only: Option<String>) {
        if self.probing.is_none() {
            self.probing = Some(Probe::start(ctx, only));
        }
    }

    fn collect(&mut self) {
        let Some(result) = self.probing.as_ref().and_then(|p| p.done.try_recv().ok()) else {
            return;
        };
        let only = self.probing.take().and_then(|p| p.only);
        match (result, only) {
            (Ok(fresh), Some(_)) => self.overview.merge(fresh),
            (Ok(fresh), None) => self.overview = fresh,
            (Err(e), _) => {
                self.error = Some(e);
                return;
            }
        }
        self.error = None;
        self.probed_at = Some(Instant::now());
    }

    fn bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Machines, "Machines");
            ui.selectable_value(&mut self.tab, Tab::People, "People");
            ui.separator();
            if ui
                .add_enabled(self.probing.is_none(), egui::Button::new("Probe all"))
                .clicked()
            {
                self.probe(ui.ctx(), None);
            }
            match (&self.probing, self.probed_at) {
                (Some(p), _) => {
                    ui.spinner();
                    ui.label(format!(
                        "probing {}",
                        p.only.as_deref().unwrap_or("every machine")
                    ));
                }
                (None, Some(at)) => {
                    ui.weak(format!("probed {} ago", ago(at.elapsed())));
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                }
                (None, None) => {}
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e.to_string());
            }
        });
    }

    fn machines(&mut self, ui: &mut Ui) {
        egui::Grid::new("machines")
            .striped(true)
            .spacing([18.0, 8.0])
            .show(ui, |ui| {
                ui.strong("machine");
                ui.strong("set up by");
                for area in Area::ALL {
                    ui.strong(area.name());
                }
                ui.end_row();
                for r in &self.overview.machines {
                    let chosen = self.selected.as_deref() == Some(r.machine.as_str());
                    if ui.selectable_label(chosen, &r.machine).clicked() {
                        self.selected = (!chosen).then(|| r.machine.clone());
                    }
                    ui.label(r.provisioned.to_string());
                    for area in Area::ALL {
                        area_cell(ui, r, area);
                    }
                    ui.end_row();
                }
            });
    }

    fn people(&self, ui: &mut Ui) {
        let machines = &self.overview.machines;
        egui::Grid::new("people")
            .striped(true)
            .spacing([18.0, 8.0])
            .show(ui, |ui| {
                ui.strong("person");
                for r in machines {
                    ui.strong(&r.machine);
                }
                ui.end_row();
                for who in &self.overview.people {
                    ui.label(who);
                    for r in machines {
                        standing_cell(ui, r, who);
                    }
                    ui.end_row();
                }
                ui.label("keys of nobody");
                for r in machines {
                    match r.access.strangers.len() {
                        0 => ui.weak("none"),
                        n => ui
                            .colored_label(ui.visuals().error_fg_color, n.to_string())
                            .on_hover_text(r.access.strangers.join("\n")),
                    };
                }
                ui.end_row();
            });
    }

    fn detail(&mut self, ui: &mut Ui, r: &Report) {
        ui.horizontal(|ui| {
            ui.heading(&r.machine);
            if ui
                .add_enabled(self.probing.is_none(), egui::Button::new("Probe again"))
                .clicked()
            {
                self.probe(ui.ctx(), Some(r.machine.clone()));
            }
            if ui.button("Close").clicked() {
                self.selected = None;
            }
        });
        ui.label(format!("Set up by {}.", r.provisioned));
        if let Some(why) = &r.unprobed {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Not probed: {why}"));
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            section(ui, "Names");
            for p in &r.paths {
                mark(
                    ui,
                    p.problem.is_none(),
                    &p.name,
                    p.problem.as_deref().unwrap_or("answers on port 22"),
                );
            }
            section(ui, "People");
            for (who, standing) in &r.access.people {
                mark(
                    ui,
                    matches!(standing, Standing::Key | Standing::Tailnet),
                    who,
                    standing.name(),
                );
            }
            for fp in &r.access.strangers {
                mark(ui, false, "nobody listed", fp);
            }
            section(ui, "Checks");
            for f in &r.findings {
                mark(ui, f.ok, f.area.name(), &f.detail);
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.collect();
        egui::Panel::top("bar").show(ui, |ui| self.bar(ui));
        let chosen = self
            .selected
            .as_deref()
            .and_then(|name| self.overview.machine(name))
            .cloned();
        if let Some(r) = chosen {
            egui::Panel::right("detail")
                .resizable(true)
                .default_size(420.0)
                .show(ui, |ui| self.detail(ui, &r));
        }
        egui::CentralPanel::default_margins().show(ui, |ui| match self.tab {
            Tab::Machines => self.machines(ui),
            Tab::People => self.people(ui),
        });
    }
}

fn area_cell(ui: &mut Ui, r: &Report, area: Area) {
    match (r.finding(area), &r.unprobed) {
        (Some(f), _) => {
            verdict(ui, f.ok).on_hover_text(&f.detail);
        }
        (None, Some(why)) => {
            ui.weak("?").on_hover_text(format!("not probed: {why}"));
        }
        (None, None) => {
            ui.weak("·").on_hover_text("not asked of this machine");
        }
    }
}

fn standing_cell(ui: &mut Ui, r: &Report, who: &str) {
    let text = |s: Standing| RichText::new(s.name());
    match r.access.people.get(who) {
        Some(s @ (Standing::Key | Standing::Tailnet)) => ui.label(text(*s).color(GOOD)),
        Some(s @ Standing::Missing) => ui.label(text(*s).color(ui.visuals().error_fg_color)),
        Some(s @ Standing::Unlisted) => ui.label(text(*s).color(ui.visuals().warn_fg_color)),
        None if r.unprobed.is_some() => ui.weak("?"),
        None => ui.weak("·"),
    };
}

fn verdict(ui: &mut Ui, ok: bool) -> egui::Response {
    match ok {
        true => ui.label(RichText::new("ok").color(GOOD)),
        false => ui.label(
            RichText::new("no")
                .strong()
                .color(ui.visuals().error_fg_color),
        ),
    }
}

fn mark(ui: &mut Ui, ok: bool, what: &str, detail: &str) {
    ui.horizontal_wrapped(|ui| {
        verdict(ui, ok);
        ui.strong(what);
        ui.label(detail);
    });
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).heading().size(15.0));
}

fn ago(d: Duration) -> String {
    match d.as_secs() {
        s @ 0..60 => format!("{s}s"),
        s => format!("{}m", s / 60),
    }
}
