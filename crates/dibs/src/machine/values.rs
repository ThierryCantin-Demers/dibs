use crate::{
    caller::Caller,
    cli::{PortName, Service},
};
use dibs_format::{
    Alias, Label, Mode,
    wire::{self, Request},
};

/// Where `--max` came from, which decides whether history may raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxFrom {
    Given,
    Default,
}

/// The card a call is pinned to, as the machine selects it.
#[derive(Debug, Clone, Default)]
pub struct Card {
    /// The alias `--device` named.
    pub alias: String,
    pub pci: String,
    /// Its runtimes, comma separated.
    pub runtimes: String,
    pub chip: String,
    /// How many cards there share its chip id.
    pub twins: usize,
}

impl Card {
    /// No card named: the machine's runtime picks.
    pub fn none() -> Card {
        Card {
            twins: 1,
            ..Card::default()
        }
    }
}

/// One call's values, which the runner is sent as its request.
#[derive(Debug, Clone)]
pub struct CallValues {
    pub mode: Mode,
    pub label: Label,
    pub wait: Option<u64>,
    pub max: u64,
    pub max_from: MaxFrom,
    pub verbose: bool,
    pub json: bool,
    pub card: Card,
    /// The whole output rather than its digest: `--stream`, or `DIBS_STREAM=1`.
    pub stream: bool,
    pub ready_within: u32,
    pub fingerprint: Option<String>,
    pub command: String,
    pub tty: bool,
    pub caller: Caller,
    /// The batch this call is a step of, and what is still to come.
    pub batch: String,
    pub ports: Vec<PortName>,
    pub services: Vec<Service>,
    /// `--new-series`.
    pub new_series: bool,
    /// The tree a recipe's job runs in.
    pub tree: Option<wire::Tree>,
}

/// How the runner learns its caller is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watch {
    /// Nothing to watch: the caller's death reaches it some other way, or not at all.
    pub off: bool,
    pub hold: bool,
    /// Seconds of silence that count as gone; 0 waits for the channel to close.
    pub lease: u64,
}

impl CallValues {
    /// The values as the runner is asked for them.
    pub fn request(&self, watch: Watch) -> Request {
        let some = |text: &str| Some(text.to_string()).filter(|t| !t.is_empty());
        let card = &self.card;
        Request {
            mode: self.mode,
            label: self.label.clone(),
            command: self.command.clone(),
            wait: self.wait,
            max: self.max,
            max_from: match self.max_from {
                MaxFrom::Given => wire::MaxFrom::Given,
                MaxFrom::Default => wire::MaxFrom::Default,
            },
            verbose: self.verbose,
            json: self.json,
            stream: self.stream,
            tty: self.tty,
            card: some(&card.alias).map(|alias| wire::Card {
                alias: Alias::new(alias),
                pci: some(&card.pci),
                runtimes: card
                    .runtimes
                    .split(',')
                    .filter(|r| !r.is_empty())
                    .map(str::to_string)
                    .collect(),
                chip: some(&card.chip),
                twins: card.twins,
            }),
            fingerprint: self.fingerprint.clone().filter(|f| !f.is_empty()),
            agent: self.caller.name.clone(),
            agent_id: self.caller.id.clone(),
            batch: some(&self.batch),
            watch: wire::Watch {
                off: watch.off,
                hold: watch.hold,
                lease: watch.lease,
            },
            new_series: self.new_series,
            ports: self.ports.iter().map(|p| p.0.clone()).collect(),
            services: self
                .services
                .iter()
                .map(|s| wire::Service {
                    name: s.name.0.clone(),
                    command: s.command.clone(),
                    ready: s.ready.clone().filter(|r| !r.is_empty()),
                })
                .collect(),
            tree: self.tree.clone(),
            ready_within: u64::from(self.ready_within),
        }
    }
}
