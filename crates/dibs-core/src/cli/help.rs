use dibs_format::Exit;

/// What `--help` prints.
pub struct Help;

impl Help {
    /// The recipe layer's own, after a recipe verb.
    pub const RECIPES: &'static str = include_str!("recipe-help.txt");

    const INTERFACE: &'static str = include_str!("help.txt");

    /// The exits a caller acts on, one row per line of the help.
    const EXITS: [&'static [Exit]; 3] = [
        &[Exit::Unreachable, Exit::NoRoom, Exit::NoLock, Exit::Busy],
        &[Exit::Cancelled, Exit::ServiceFailed, Exit::Overran],
        &[Exit::TargetRebuilt],
    ];

    /// `dibs --help`: the interface, then what each exit means.
    pub fn text() -> String {
        let mut text = Help::INTERFACE.to_string();
        for (row, exits) in Help::EXITS.iter().enumerate() {
            let lead = match row {
                0 => "  exit ",
                _ => "       ",
            };
            let cells: Vec<String> = exits
                .iter()
                .map(|e| format!("{} {}", e.code(), e.meaning()))
                .collect();
            text.push_str(&format!("{lead}{}\n", cells.join("    ")));
        }
        text.push('\n');
        text
    }
}
