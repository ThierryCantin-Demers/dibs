use crate::{
    lock::LockDir,
    platform::{Host, Platform as _},
};
use dibs_format::wire::Picked;
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{ErrorKind, Read as _, Write as _},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener},
    str::FromStr,
};

/// Tries per name before the range counts as full.
const TRIES: usize = 200;

/// The ports `--port` may be given: `DIBS_PORTS`, `lo-hi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub low: u16,
    pub high: u16,
}

/// What a call's `--port` names were given. Each port is reserved by an exclusive `port.<n>`
/// naming this process, so two calls at once are never given the same one, and prune clears it
/// once the process is gone.
pub struct Ports {
    pub picked: Vec<Picked>,
    /// The first name the range had no port for, and every name after it.
    pub unpicked: Vec<String>,
}

impl Ports {
    pub fn take(names: &[String], range: PortRange, dir: &LockDir, pid: u32) -> Ports {
        let mut random = Random::open();
        let listening = Host::listening();
        let mut picked: Vec<Picked> = Vec::new();
        for (at, name) in names.iter().enumerate() {
            let port = (0..TRIES)
                .map(|_| range.low + random.below(u32::from(range.high - range.low) + 1) as u16)
                .find(|port| {
                    !picked.iter().any(|p| p.port == *port)
                        && !listening.contains(port)
                        && free(*port)
                        && reserve(dir, *port, pid)
                });
            match port {
                Some(port) => picked.push(Picked {
                    name: name.clone(),
                    port,
                }),
                None => {
                    return Ports {
                        picked,
                        unpicked: names[at..].to_vec(),
                    };
                }
            }
        }
        Ports {
            picked,
            unpicked: Vec::new(),
        }
    }

    pub fn complete(&self) -> bool {
        self.unpicked.is_empty()
    }

    /// The port a `--ready tcp:` names: a picked port by its name, or the number given.
    pub fn of(&self, named: &str) -> Option<u16> {
        self.picked
            .iter()
            .find(|p| p.name == named)
            .map(|p| p.port)
            .or_else(|| named.parse().ok())
    }

    /// The trailer's lines for them, in the order they were asked for.
    pub fn lines(&self, host: &str) -> String {
        let picked = self
            .picked
            .iter()
            .map(|p| format!("  port {}: {} on {host}\n", p.name, p.port));
        let unpicked = self
            .unpicked
            .iter()
            .map(|name| format!("  port {name}: none free on {host}\n"));
        picked.chain(unpicked).collect()
    }
}

/// Nothing is bound to it on either family's wildcard, which a listener missing from the
/// table, in another namespace say, would show, nor on loopback: macOS lets a wildcard bind share
/// a port with a server listening on 127.0.0.1 alone.
fn free(port: u16) -> bool {
    let bound = |address: IpAddr| match TcpListener::bind((address, port)) {
        Ok(_) => false,
        Err(e) => e.kind() == ErrorKind::AddrInUse,
    };
    [
        Ipv4Addr::UNSPECIFIED.into(),
        Ipv6Addr::UNSPECIFIED.into(),
        Ipv4Addr::LOCALHOST.into(),
        Ipv6Addr::LOCALHOST.into(),
    ]
    .into_iter()
    .all(|address| !bound(address))
}

fn reserve(dir: &LockDir, port: u16, pid: u32) -> bool {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.file("port", u32::from(port)))
        .and_then(|mut file| file.write_all(format!("{pid}\n").as_bytes()))
        .is_ok()
}

/// Numbers from `/dev/urandom`, or from the clock where it cannot be read.
struct Random {
    source: Option<File>,
    fallback: u64,
}

impl Random {
    fn open() -> Random {
        let fallback = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            ^ u64::from(std::process::id());
        Random {
            source: File::open("/dev/urandom").ok(),
            fallback,
        }
    }

    fn below(&mut self, bound: u32) -> u32 {
        let mut bytes = [0u8; 4];
        let read = self
            .source
            .as_mut()
            .is_some_and(|f| f.read_exact(&mut bytes).is_ok());
        let value = match read {
            true => u32::from_ne_bytes(bytes),
            false => {
                self.fallback = self
                    .fallback
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (self.fallback >> 33) as u32
            }
        };
        value % bound.max(1)
    }
}

/// `DIBS_PORTS` did not read as `lo-hi`.
#[derive(Debug)]
pub struct BadRange;

impl FromStr for PortRange {
    type Err = BadRange;

    fn from_str(text: &str) -> Result<PortRange, BadRange> {
        let (low, high) = text.split_once('-').ok_or(BadRange)?;
        let low: u16 = low.trim().parse().map_err(|_| BadRange)?;
        let high: u16 = high.trim().parse().map_err(|_| BadRange)?;
        match low <= high && low > 0 {
            true => Ok(PortRange { low, high }),
            false => Err(BadRange),
        }
    }
}

impl Default for PortRange {
    fn default() -> PortRange {
        PortRange {
            low: 20500,
            high: 20999,
        }
    }
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.low, self.high)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_reads_as_low_dash_high() {
        assert_eq!(
            "20500-20999".parse::<PortRange>().ok(),
            Some(PortRange::default())
        );
        assert!("20999-20500".parse::<PortRange>().is_err());
        assert!("20500".parse::<PortRange>().is_err());
    }

    #[test]
    fn a_port_in_use_is_not_free() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!free(port));
        drop(listener);
    }
}
