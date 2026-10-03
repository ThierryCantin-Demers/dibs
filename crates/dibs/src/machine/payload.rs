use crate::{
    caller::Caller,
    cli::{BashQuoted, PortName, Service},
};
use dibs_format::{Label, Mode};
use flate2::{Compression, write::GzEncoder};
use std::{fmt::Write as _, io::Write as _};

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

/// One call's values, which the machine half reads ahead of its own code.
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
}

/// How the machine half learns its caller is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watch {
    /// Nothing to watch: the caller's death reaches it some other way, or not at all.
    pub off: bool,
    pub hold: bool,
    /// Seconds of silence that count as gone; 0 waits for the channel to close.
    pub lease: u64,
}

impl CallValues {
    /// The values as assignments, then the machine half.
    pub fn script(&self, watch: Watch) -> String {
        let flag = |b: bool| if b { "1" } else { "0" };
        let max_from = match self.max_from {
            MaxFrom::Given => "given",
            MaxFrom::Default => "default",
        };
        let twins = self.card.twins.to_string();
        let max = self.max.to_string();
        let wait = self.wait.map(|w| w.to_string()).unwrap_or_default();
        let ready_within = self.ready_within.to_string();
        let fingerprint = self.fingerprint.as_deref().unwrap_or_default();
        let lease = watch.lease.to_string();
        let values: [(&str, &str); 23] = [
            ("MODE", self.mode.as_str()),
            ("LABEL", self.label.as_str()),
            ("WAIT", &wait),
            ("MAXHOLD", &max),
            ("VERBOSE", flag(self.verbose)),
            ("JSON", flag(self.json)),
            ("DEV_PCI", &self.card.pci),
            ("DEV_RT", &self.card.runtimes),
            ("DEV_CHIP", &self.card.chip),
            ("DEV_TWINS", &twins),
            ("STREAM", flag(self.stream)),
            ("READY_WITHIN", &ready_within),
            ("MAXFROM", max_from),
            ("FINGERPRINT", fingerprint),
            ("CMD", &self.command),
            ("TTY", flag(self.tty)),
            ("AGENT", &self.caller.name),
            ("AGENT_ID", &self.caller.id),
            ("DEV_NAME", &self.card.alias),
            ("BATCH", &self.batch),
            ("NO_WATCH", flag(watch.off)),
            ("HOLD", flag(watch.hold)),
            ("LEASE", &lease),
        ];
        let mut script = String::new();
        for (name, value) in values {
            let _ = writeln!(script, "{name}={}", BashQuoted(value));
        }
        let names: Vec<&str> = self.services.iter().map(|s| s.name.0.as_str()).collect();
        let ready: Vec<&str> = self
            .services
            .iter()
            .map(|s| s.ready.as_deref().unwrap_or_default())
            .collect();
        let commands: Vec<&str> = self.services.iter().map(|s| s.command.as_str()).collect();
        let ports: Vec<&str> = self.ports.iter().map(|p| p.0.as_str()).collect();
        for (name, members) in [
            ("PORT_NAME", ports),
            ("WITH_NAME", names),
            ("WITH_READY", ready),
            ("WITH_CMD", commands),
        ] {
            let members: Vec<String> = members
                .iter()
                .enumerate()
                .map(|(i, m)| format!("[{i}]={}", BashQuoted(m)))
                .collect();
            let _ = writeln!(script, "declare -a {name}=({})", members.join(" "));
        }
        script.push_str(MACHINE_HALF);
        script
    }
}

/// `lib/machine`, the half of dibs every call ships to the machine it runs on, as it was when
/// this binary was built.
const MACHINE_HALF: &str = include_str!(concat!(env!("OUT_DIR"), "/machine-half.sh"));

/// A script as it crosses ssh: compressed, then base64 on one line.
pub fn encode(script: &str) -> String {
    let mut gz = GzEncoder::new(Vec::new(), Compression::best());
    let _ = gz.write_all(script.as_bytes());
    let bytes = gz.finish().unwrap_or_default();
    base64(&bytes)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            match i <= chunk.len() {
                true => out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63])),
                false => out.push('='),
            }
        }
    }
    out
}

/// What `base64` printed, back as bytes; None when it is not base64.
pub fn decode(text: &[u8]) -> Option<Vec<u8>> {
    let digits: Vec<u8> = text
        .iter()
        .filter(|b| !b.is_ascii_whitespace() && **b != b'=')
        .map(|b| ALPHABET.iter().position(|a| a == b).map(|p| p as u8))
        .collect::<Option<_>>()?;
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, d)| n | u32::from(*d) << (18 - 6 * i));
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_encoded_decodes_back() {
        for text in [
            "",
            "f",
            "fo",
            "foo",
            "foobar",
            "a longer line\n with a newline",
        ] {
            assert_eq!(
                decode(base64(text.as_bytes()).as_bytes()).as_deref(),
                Some(text.as_bytes())
            );
        }
        assert_eq!(decode(b"Zm9v\nYmFy\n").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(decode(b"not base64!"), None);
    }

    #[test]
    fn base64_pads_as_the_tool_does() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
