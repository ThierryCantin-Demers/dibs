use crate::probe::{base::Report, facts::Tools};
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

const DEVICES: &str = "/sys/bus/pci/devices";
/// lspci lines a GPU shows as.
const GPU_CLASSES: [&str; 3] = [
    "vga compatible controller",
    "3d controller",
    "display controller",
];
/// Words of a card's name that every card of its vendor shares, left out of its alias.
const VENDOR_WORDS: [&str; 7] = [
    "nvidia",
    "geforce",
    "corporation",
    "advanced micro devices",
    "radeon",
    "intel",
    " arc ",
];

/// The GPUs, as their vendors' tools see them. Judged on what a tool prints, never on its being
/// installed or its exit: rocm-smi on a machine with no AMD card prints an error and exits 0.
pub struct Gpus {
    /// `name, bus id, memory, compute capability` per card nvidia-smi sees.
    nvidia: Vec<Vec<String>>,
    /// Card names rocm-smi sees.
    amd: Vec<String>,
    /// The HIP runtime is installed, which a ROCm card needs, rocm-smi alone not being it.
    hip: bool,
}

/// What a card gets to the host: the narrowest link on its path to the root, against what the
/// card itself can do, by width; the speed is the link's ceiling, since an idle card downshifts.
struct Link {
    to_host: u32,
    card: u32,
    speed: String,
}

impl Gpus {
    pub fn find(report: &mut Report) -> Gpus {
        let nvidia: Vec<Vec<String>> = Tools::here()
            .output(
                "nvidia-smi",
                &[
                    "--query-gpu=name,pci.bus_id,memory.total,compute_cap",
                    "--format=csv,noheader,nounits",
                ],
            )
            .unwrap_or_default()
            .lines()
            .map(|line| line.split(',').map(|f| f.trim().to_string()).collect())
            .collect();
        for card in &nvidia {
            let field = |n: usize| card.get(n).map_or("", String::as_str);
            report.line(&format!(
                "    gpu   {}  {}  {} MiB  sm{}",
                field(0),
                field(1),
                field(2),
                field(3)
            ));
        }
        let amd: Vec<String> = Tools::here()
            .output("rocm-smi", &["--showproductname", "--csv"])
            .unwrap_or_default()
            .lines()
            .skip(1)
            .filter_map(|line| {
                let fields: Vec<&str> = line.split(',').collect();
                (fields.len() > 1).then(|| fields[1].to_string())
            })
            .collect();
        let hip = Tools::here().find("rocminfo").is_some()
            || Tools::here()
                .output("ldconfig", &["-p"])
                .is_some_and(|l| l.contains("libamdhip64"));
        for card in &amd {
            let runtime = match hip {
                true => "(rocm)",
                false => "(no rocm runtime)",
            };
            report.line(&format!("    gpu   {card}  {runtime}"));
        }
        if !amd.is_empty() && !hip {
            report.warn("rocm-smi sees these cards but nothing here can run HIP on them:");
            report.note("no rocminfo and no libamdhip64. They are Vulkan-only until the ROCm");
            report.note("runtime is installed, so a rocm label would have nowhere to go.");
        }
        for bus in display_devices() {
            let Some(link) = Link::of(&bus).filter(|l| l.to_host < l.card) else {
                continue;
            };
            report.warn(&format!(
                "{bus} reaches the host over x{}, and the card can do x{}",
                link.to_host, link.card
            ));
            report.note(&format!(
                "Host transfers cost {}x what the card allows, so a benchmark",
                link.card / link.to_host
            ));
            report.note("that moves data is measuring the riser or the slot it is in.");
        }
        if nvidia.is_empty() && amd.is_empty() {
            match Tools::here().find("lspci").is_some() {
                true => {
                    let seen: Vec<String> = lspci_gpus(&["vga", "3d controller"])
                        .into_iter()
                        .take(8)
                        .collect();
                    for line in seen {
                        report.line(&format!("    gpu?  {line}"));
                    }
                    report.note(
                        "seen by lspci only: no vendor tool here can talk to them, so nothing",
                    );
                    report.note("can be probed and no GPU work can be routed to this machine yet.");
                }
                false => report.warn(
                    "no nvidia-smi, no rocm-smi, no lspci: cannot tell what is in this machine",
                ),
            }
        }
        Gpus { nvidia, amd, hip }
    }

    /// The inventory's device tables, from what was just detected: a hand-written bus id is how
    /// an inventory goes quietly stale.
    pub fn entries(&self) -> String {
        let vulkan = Tools::here().find("vulkaninfo").is_some();
        let lines = lspci_gpus(&GPU_CLASSES);
        let chips: Vec<String> = lines.iter().filter_map(|l| chip(l)).collect();
        let mut out = String::new();
        for line in &lines {
            let Some(ids) = chip(line) else {
                continue;
            };
            let first = line.split(' ').next().unwrap_or_default();
            let bus = match first.matches(':').count() {
                2 => first.to_string(),
                _ => format!("0000:{first}"),
            };
            let seen = self
                .nvidia
                .iter()
                .find(|card| {
                    card.get(1)
                        .is_some_and(|id| id.to_lowercase().contains(&bus.to_lowercase()))
                })
                .and_then(|card| card.first().cloned());
            let short = seen.clone().unwrap_or_else(|| short_name(line));
            let driver = fs::read_link(Path::new(DEVICES).join(&bus).join("driver")).is_ok();
            let mut runtimes = Vec::new();
            match ids.split(':').next() {
                Some("10de") if seen.is_some() => runtimes.push("\"cuda\""),
                Some("1002") if self.hip && !self.amd.is_empty() => runtimes.push("\"rocm\""),
                _ => {}
            }
            if vulkan && driver {
                runtimes.push("\"vulkan\"");
            }
            let mut slug = slug(&short);
            if slug.is_empty() {
                slug = bus.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            }
            if chips.iter().filter(|c| **c == ids).count() > 1 {
                let slot = bus.split_once(':').map_or("", |(_, rest)| rest);
                slug = format!("{slug}.{}", slot.split(':').next().unwrap_or_default());
            }
            let _ = write!(
                out,
                "\n  [[machine.@NAME@.device]]\n  kind     = \"gpu\"\n  alias    = \"gpu:{slug}\"\n  name     = \"{short}\"\n  pci      = \"{bus}\"\n  chip     = \"{ids}\"\n"
            );
            if let Some(link) = Link::of(&bus) {
                let _ = writeln!(
                    out,
                    "  link     = \"x{} of x{} at {}\"",
                    link.to_host, link.card, link.speed
                );
            }
            let _ = writeln!(out, "  runtimes = [{}]", runtimes.join(", "));
        }
        out
    }
}

impl Link {
    fn of(bus: &str) -> Option<Link> {
        let devices = Path::new(DEVICES);
        let read = |dir: &Path, file: &str| -> Option<String> {
            Some(fs::read_to_string(dir.join(file)).ok()?.trim().to_string())
        };
        let card: u32 = read(&devices.join(bus), "max_link_width")?.parse().ok()?;
        let mut narrowest: Option<u32> = None;
        let mut slowest: Option<(u32, String)> = None;
        let mut at: Option<PathBuf> = Some(devices.join(bus));
        while let Some(dir) = at.take().filter(|d| d.exists()) {
            if let Some(width) = read(&dir, "current_link_width").and_then(|w| w.parse().ok()) {
                narrowest = Some(narrowest.map_or(width, |n: u32| n.min(width)));
            }
            if let Some(speed) = read(&dir, "max_link_speed") {
                let whole: Option<u32> = speed.split('.').next().and_then(|s| s.parse().ok());
                if let Some(whole) = whole
                    && slowest.as_ref().is_none_or(|(s, _)| whole < *s)
                {
                    let shown = speed.trim_end_matches(" PCIe").replace(' ', "");
                    slowest = Some((whole, shown));
                }
            }
            let up = fs::canonicalize(dir.join("..")).ok();
            at = up
                .filter(|u| {
                    u.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("0000:"))
                })
                .map(|u| devices.join(u.file_name().unwrap_or_default()));
        }
        let to_host = narrowest.filter(|w| *w >= 1)?;
        (1..=32).contains(&card).then(|| Link {
            to_host,
            card,
            speed: slowest.map_or("unknown".to_string(), |(_, s)| s),
        })
    }
}

/// The slots holding a display device.
fn display_devices() -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(DEVICES)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("current_link_width").exists())
        .filter(|e| {
            fs::read_to_string(e.path().join("class")).is_ok_and(|c| c.starts_with("0x030"))
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

/// `lspci -nn` lines naming any of the classes, case aside.
fn lspci_gpus(classes: &[&str]) -> Vec<String> {
    Tools::here()
        .output("lspci", &["-nn"])
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            let lower = l.to_lowercase();
            classes.iter().any(|c| lower.contains(c))
        })
        .map(str::to_string)
        .collect()
}

/// The last `[vendor:device]` on an lspci line.
fn chip(line: &str) -> Option<String> {
    line.match_indices('[')
        .filter_map(|(at, _)| {
            let id = line.get(at + 1..at + 10)?;
            let hex = |s: &str| s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit());
            (line.get(at + 10..at + 11) == Some("]")
                && id.split_once(':').is_some_and(|(v, d)| hex(v) && hex(d)))
            .then(|| id.to_string())
        })
        .next_back()
}

/// What lspci calls a card: past the class, before the chip id, inside its last brackets.
fn short_name(line: &str) -> String {
    let described = line.split_once("]: ").map_or(line, |(_, rest)| rest);
    let described = match chip(described) {
        Some(id) => described
            .find(&format!(" [{id}]"))
            .map_or(described, |at| &described[..at]),
        None => described,
    };
    match (described.rfind('['), described.rfind(']')) {
        (Some(open), Some(close)) if open < close => described[open + 1..close].to_string(),
        _ => described.to_string(),
    }
}

/// A card's name as an alias: lowercase, its vendor's words gone, letters and digits only.
fn slug(name: &str) -> String {
    let mut lower = name.to_lowercase();
    for word in VENDOR_WORDS {
        lower = lower.replace(word, if word == " arc " { " " } else { "" });
    }
    lower
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTX: &str = "7f:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD102 [GeForce RTX 4090] [10de:2684] (rev a1)";

    #[test]
    fn an_lspci_line_gives_its_chip_and_its_card() {
        assert_eq!(chip(RTX).as_deref(), Some("10de:2684"));
        assert_eq!(short_name(RTX), "GeForce RTX 4090");
        assert_eq!(slug("NVIDIA GeForce RTX 4090"), "rtx4090");
        assert_eq!(slug("Intel Arc A770"), "a770");
    }
}
