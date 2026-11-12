use crate::{
    caller::Caller,
    cli::{PortName, Service},
    machine::Card,
};
use dibs_format::{
    Alias, Label, Mode,
    wire::{self, Request},
};
use std::collections::BTreeMap;

/// Where `--max` came from, which decides whether history may raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxFrom {
    Given,
    Default,
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
    /// A status that reads the machine's CPU, memory and cards too.
    pub metrics: bool,
    /// Each card's alias by its PCI address, for those metrics to name it by.
    pub aliases: BTreeMap<String, String>,
    /// `--preemptible`, or a recipe's step that only compiles.
    pub preempt: bool,
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
    fn wire_card(card: &Card, alias: &str) -> wire::Card {
        let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
        wire::Card {
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
        }
    }

    /// Answered beside whatever holds the machine, so as never to wait behind it.
    pub fn takes_no_lock(&self) -> bool {
        matches!(
            self.mode,
            Mode::Peek
                | Mode::Status
                | Mode::Watch
                | Mode::Log
                | Mode::Out
                | Mode::Kill
                | Mode::KillForce
                | Mode::Release
                | Mode::Close
                | Mode::Open
        )
    }

    /// Closing and opening a machine are left out: a runner from before they existed cannot read
    /// the call, and neither has anything to look at or stop behind a held lock.
    pub fn an_older_runner_answers(&self) -> bool {
        self.takes_no_lock() && !matches!(self.mode, Mode::Close | Mode::Open)
    }

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
            // Each card goes as its own alias; the runner names the pin by all of them.
            card: some(&card.alias).map(|alias| {
                CallValues::wire_card(card, alias.split(',').next().unwrap_or_default())
            }),
            also: card
                .also
                .iter()
                .map(|c| CallValues::wire_card(c, &c.alias))
                .collect(),
            fingerprint: self.fingerprint.clone().filter(|f| !f.is_empty()),
            agent: self.caller.name.clone(),
            agent_id: self.caller.id.clone(),
            subagent: self.caller.subagent.clone(),
            batch: some(&self.batch),
            watch: wire::Watch {
                off: watch.off,
                hold: watch.hold,
                lease: watch.lease,
            },
            new_series: self.new_series,
            metrics: self.metrics,
            aliases: self.aliases.clone(),
            preempt: self.preempt,
            // From the environment, which `--when-open` sets, so a batch's steps and a sync's
            // transfer carry it too.
            when_open: std::env::var("DIBS_WHEN_OPEN").is_ok_and(|v| v == "1"),
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
