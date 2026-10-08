//! Which card each label's measurements were taken on here, kept on the machine so that every
//! caller's runs keep to it, not only one laptop's. It refuses jobs, so it is off until the
//! machine's `machine_series` setting turns it on.

use crate::{call::Received, clock::Moment, shared::SharedFile};
use std::{fmt, fs, path::PathBuf};

/// A file without this first line is from another keying and is ignored whole.
const HEADER: &str = "#dibs-cards 1";

/// The binding of the call's label to a card, in the file `cards` beside the history.
pub struct Binding<'a> {
    pub path: PathBuf,
    pub call: &'a Received,
    pub host: &'a str,
}

/// A run on another card than its label's series here.
#[derive(Debug)]
pub struct OtherCard {
    pub label: String,
    pub host: String,
    pub before: String,
    pub by: String,
    pub now: String,
}

impl fmt::Display for OtherCard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dibs: '{}' has been measured on another card of {}.\n  \
             before:  {}, by {}\n  \
             now:     {}\n  \
             Those are two histories, not one series, and a number from one cannot be\n  \
             compared against a number from the other. Use the card it was measured on, or\n  \
             start its series on this machine again deliberately:  --new-series\n",
            self.label, self.host, self.before, self.by, self.now
        )
    }
}

/// One line: label, card, who ran it, when, and how many runs.
struct Line<'a> {
    fields: Vec<&'a str>,
}

impl<'a> Line<'a> {
    fn of(text: &'a str) -> Line<'a> {
        Line {
            fields: text.split('\t').collect(),
        }
    }

    fn field(&self, at: usize) -> &'a str {
        self.fields.get(at).copied().unwrap_or_default()
    }
}

impl Binding<'_> {
    fn card(&self) -> String {
        self.call
            .request
            .card
            .as_ref()
            .map_or("none".to_string(), |c| c.alias.to_string())
    }

    fn label(&self) -> &str {
        self.call.label().as_str()
    }

    /// The refusal of a run on another card than the label's series here, unless it starts the
    /// series again on purpose.
    pub fn check(&self) -> Result<(), OtherCard> {
        if self.call.request.new_series {
            return Ok(());
        }
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        if text.lines().next() != Some(HEADER) {
            return Ok(());
        }
        let card = self.card();
        let Some(line) = text
            .lines()
            .skip(1)
            .map(Line::of)
            .find(|l| l.field(0) == self.label())
            .filter(|l| l.field(1) != card)
        else {
            return Ok(());
        };
        Err(OtherCard {
            label: self.label().to_string(),
            host: self.host.to_string(),
            before: line.field(1).to_string(),
            by: line.field(2).to_string(),
            now: card,
        })
    }

    /// Files a run that measured something, by whoever ran it.
    pub fn record(&self) {
        let card = self.card();
        let _ = SharedFile { path: &self.path }.rewrite(|text| {
            let text = match text.lines().next() == Some(HEADER) {
                true => text,
                false => "",
            };
            let mut runs = 0;
            let mut written = format!("{HEADER}\n");
            for line in text.lines().skip(1) {
                let fields = Line::of(line);
                match fields.field(0) == self.label() {
                    true if !self.call.request.new_series && fields.field(1) == card => {
                        runs = fields.field(4).parse().unwrap_or(0);
                    }
                    true => {}
                    false => {
                        written.push_str(line);
                        written.push('\n');
                    }
                }
            }
            written.push_str(&format!(
                "{}\t{card}\t{}\t{}\t{}\n",
                self.label(),
                self.call.agent,
                Moment::epoch_now(),
                runs + 1
            ));
            Some(written)
        });
    }
}
