use crate::{
    Alias, Label, Mode,
    wire::{Bundle, Tree},
};
use serde::{Deserialize, Serialize};

/// One call, as the runner is asked to make it.
///
/// # Guarantees
///
/// - An older runner reads a newer client's request for a call that takes no lock: no field or
///   variant such a call uses is renamed or removed, and an older runner passes over a field it
///   does not know.
/// - The newest runner reads an older client's request for such a call: a field added since
///   reads as absent, through `#[serde(default)]` or an `Option`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub mode: Mode,
    /// As the machine files it.
    pub label: Label,
    /// One shell string, run by `bash -c`; for a close, the text [`Close::to_text`] writes, and
    /// for a gc, the gc's words.
    pub command: String,
    #[serde(rename = "wait")]
    pub wait_secs: Option<u64>,
    /// How long the job may hold the lock; 0 is no cap.
    #[serde(rename = "max")]
    pub max_secs: u64,
    #[serde(rename = "max_from")]
    pub max_source: MaxSource,
    pub verbose: bool,
    pub json: bool,
    /// The whole output rather than its digest.
    pub stream: bool,
    /// The caller's stdout is a terminal.
    pub tty: bool,
    pub card: Option<Card>,
    /// The cards after `card` a job over several is pinned to, read in the order named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Card>,
    pub fingerprint: Option<String>,
    /// The caller's session title.
    pub agent: String,
    /// The caller's session id.
    pub agent_id: String,
    /// The person the caller runs as on its own computer, `DIBS_USER` or else `USER`: whom to
    /// ask about the job, never whose it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The Claude subagent within that session that made the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// The batch step this call is, as `batch.<pid>` holds it: the id and step, `k` of `n`, then
    /// the steps still to come.
    pub batch: Option<String>,
    pub watch: Watch,
    /// `--port` names, each given a free port the job reads as `$DIBS_PORT_<NAME>`, read in the
    /// order given.
    pub ports: Vec<String>,
    /// `--with` servers, run for the job's length, started in the order given.
    pub services: Vec<Service>,
    /// How long the services have to be ready in.
    #[serde(rename = "ready_within")]
    pub ready_within_secs: u64,
    /// The measurement starts its label's series on this machine again, on its card.
    pub new_series: bool,
    /// Only compiles, so it may start ahead of a queued benchmark and be frozen while one runs.
    #[serde(default)]
    pub preempt: bool,
    /// `--urgent`, and why: a benchmark that parks the shared jobs running and has the machine to
    /// itself, and a shared job that goes around the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<String>,
    /// Waits for a machine closed by its owners to open, rather than being refused.
    #[serde(default)]
    pub when_open: bool,
    /// For `--check`: the path the team's inventory says every machine keeps its trees under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// `--low-priority`: a shared job that starts only once nothing else runs or waits on the
    /// machine, and is stopped, to run again from the start, as soon as another job comes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub low_priority: bool,
    /// A status that reads the machine's CPU, memory and cards as well.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub metrics: bool,
    /// Each card's `--device` alias, looked up by its PCI address, from the caller's inventory,
    /// which is what a metrics reading names a card by.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub aliases: std::collections::BTreeMap<String, String>,
    /// The tree the job runs in, prepared at its head when it is not yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<Tree>,
    /// How long the job runs, frozen and parked time not counted, before its call lets go of it.
    #[serde(rename = "check_in", default, skip_serializing_if = "Option::is_none")]
    pub check_in_secs: Option<u64>,
    /// The caller's id for this attempt at the work, which the machine admits once: a request
    /// naming one it has seen is told how that one went, and never runs again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    /// Only asks how the attempt went, and never admits it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lookup: bool,
    /// An executable kept after the build, or checked before the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<Bundle>,
    /// `<ref>@sha256:<digest>`: the tree builds into a target of that image's own, and a shared
    /// job runs in the image's root filesystem, linking its C library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl Request {
    /// The cards a call is pinned to as `--device` named them, joined by `,`, which its series is
    /// filed under; None for a call pinned to no card.
    pub fn device(&self) -> Option<Alias> {
        let first = self.card.as_ref()?;
        Some(Alias::new(
            std::iter::once(first)
                .chain(&self.also)
                .map(|c| c.alias.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ))
    }
}

/// A machine taken back by its owners.
///
/// # Assumptions
///
/// - No `except` account is empty, holds a `,` or a newline, or starts or ends with whitespace.
/// - A reason is not empty, and does not start or end with whitespace.
///
/// # Guarantees
///
/// - [`Close::from_text`] reads back the close [`Close::to_text`] wrote, a newline in the reason
///   read as a space.
/// - [`Close::from_text`] passes over any line it does not know, so an empty text closes to
///   everyone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Close {
    /// Stops the jobs running rather than letting them finish.
    pub now: bool,
    /// Accounts whose work carries on, asked whether they hold an account.
    pub except: Vec<String>,
    pub reason: Option<String>,
}

impl Close {
    /// One `key=value` a line, as a `--close` call sends it and the lock directory's `closed`
    /// holds it: `now` always, then `except` and `reason` when there are any.
    pub fn to_text(&self) -> String {
        let except = Some(&self.except)
            .filter(|except| !except.is_empty())
            .map(|except| format!("except={}\n", except.join(",")));
        let reason = self
            .reason
            .as_ref()
            .map(|reason| format!("reason={}\n", reason.replace('\n', " ")));
        format!(
            "now={}\n{}{}",
            u8::from(self.now),
            except.unwrap_or_default(),
            reason.unwrap_or_default()
        )
    }

    /// The close a `--close` call sent, or the lock directory's `closed` holds.
    pub fn from_text(text: &str) -> Close {
        let mut close = Close::default();
        for line in text.lines() {
            match line.split_once('=') {
                Some(("now", value)) => close.now = value.trim() == "1",
                Some(("except", value)) => {
                    close.except = value
                        .split(',')
                        .map(str::trim)
                        .filter(|a| !a.is_empty())
                        .map(String::from)
                        .collect()
                }
                Some(("reason", value)) => {
                    close.reason = Some(value.trim().to_string()).filter(|r| !r.is_empty())
                }
                _ => {}
            }
        }
        close
    }
}

/// A server run on the machine for one call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub command: String,
    /// `tcp:[host:]port`, the port possibly a `--port` name, or a command that exits 0 once the
    /// server can be used. None is ready at once.
    pub ready: Option<String>,
}

/// Where `--max` came from, which decides whether history may raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaxSource {
    Given,
    Default,
}

/// The card a call is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub alias: Alias,
    /// The slot it was in when the machine was probed; None for a device with no slot.
    pub pci: Option<String>,
    /// The runtimes it runs, asked whether they hold one and read in order.
    pub runtimes: Vec<String>,
    /// `vendor:device`, as the machine's probe read it.
    pub chip: Option<String>,
    /// How many cards there share its chip id.
    #[serde(rename = "twins")]
    pub twin_count: usize,
}

/// How the runner learns its caller is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// Nothing to watch: the stream after the request is not the caller's.
    pub off: bool,
    /// The lock is held for a command run elsewhere, which ends with a `release`.
    pub hold: bool,
    /// How long a silence counts as gone; 0 waits for the stream to end.
    #[serde(rename = "lease")]
    pub lease_secs: u64,
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A shared job's request, which tests change the fields of that they are about.
    pub fn request() -> Request {
        Request {
            mode: Mode::Shared,
            label: Label::new("build"),
            command: "echo 'a\nb'\t$HOME".into(),
            wait_secs: None,
            max_secs: 7200,
            max_source: MaxSource::Default,
            verbose: false,
            json: false,
            stream: false,
            tty: false,
            card: None,
            also: Vec::new(),
            fingerprint: None,
            agent: "session".into(),
            agent_id: "id".into(),
            user: None,
            subagent: None,
            metrics: false,
            aliases: Default::default(),
            batch: None,
            watch: Watch {
                off: false,
                hold: false,
                lease_secs: 120,
            },
            ports: vec!["api".into()],
            services: Vec::new(),
            ready_within_secs: 300,
            new_series: false,
            preempt: false,
            when_open: false,
            workspace: None,
            low_priority: false,
            urgent: None,
            tree: None,
            check_in_secs: None,
            attempt: None,
            lookup: false,
            image: None,
            bundle: None,
        }
    }

    fn card(alias: &str) -> Card {
        Card {
            alias: Alias::new(alias),
            pci: None,
            runtimes: Vec::new(),
            chip: None,
            twin_count: 1,
        }
    }

    #[test]
    fn a_call_pinned_to_no_card_has_no_device() {
        assert_eq!(request().device(), None);
    }

    #[test]
    fn a_call_over_several_cards_names_them_all_joined_by_commas() {
        let request = Request {
            card: Some(card("gpu0")),
            also: vec![card("gpu1"), card("gpu2")],
            ..request()
        };
        assert_eq!(request.device(), Some(Alias::new("gpu0,gpu1,gpu2")));
    }

    #[test]
    fn a_close_with_nothing_optional_is_one_line() {
        let close = Close {
            now: true,
            ..Close::default()
        };
        assert_eq!(close.to_text(), "now=1\n");
        assert_eq!(Close::from_text(&close.to_text()), close);
    }

    #[test]
    fn a_close_except_accounts_is_a_line_of_them_joined_by_commas() {
        let close = Close {
            except: vec!["ana".into(), "bo".into()],
            ..Close::default()
        };
        assert_eq!(close.to_text(), "now=0\nexcept=ana,bo\n");
        assert_eq!(Close::from_text(&close.to_text()), close);
    }

    #[test]
    fn a_close_reason_is_one_line_its_newlines_written_as_spaces() {
        let close = Close {
            reason: Some("a demo\ntoday".into()),
            ..Close::default()
        };
        assert_eq!(close.to_text(), "now=0\nreason=a demo today\n");
        assert_eq!(
            Close::from_text(&close.to_text()).reason.as_deref(),
            Some("a demo today")
        );
    }

    #[test]
    fn an_empty_close_text_closes_to_everyone() {
        assert_eq!(Close::from_text(""), Close::default());
    }
}
