//! How numbers and names are written, the same way the wrapper writes them.

use ratatui::style::Color;

/// The same hues the shell uses, in the same order, so one agent is one colour in both.
const HUES: [u8; 12] = [33, 39, 63, 99, 105, 135, 170, 176, 205, 38, 44, 111];

pub fn agent_hue(name: &str) -> Color {
    let mut h: u32 = 7;
    for c in name.chars() {
        h = h.wrapping_mul(31).wrapping_add(c as u32) & 0xffff;
    }
    Color::Indexed(HUES[h as usize % HUES.len()])
}

/// Columns are fixed width and ratatui cuts without saying so, which reads as a typo
/// rather than as a truncation. The full text is always in the pane below.
pub fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut t: String = s.chars().take(width.saturating_sub(1)).collect();
    t.push('\u{2026}');
    t
}

/// Hundredths of a core, as cores. The total core-time goes in the pane below: on its own it
/// only ever prompts the question of why three minutes of work shows twenty-three of CPU.
pub fn cores(hundredths: u64) -> String {
    format!("{}.{}x", hundredths / 100, (hundredths % 100) / 10)
}

/// A control character drawn into a cell reaches the terminal as itself: a carriage return,
/// which ssh ends its messages with, moves the cursor, and the screen is garbled where ratatui
/// believes it drew text, so nothing ever redraws it.
pub fn plain(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_that_moves_the_cursor_reaches_the_screen() {
        assert_eq!(
            plain("ssh: connect to host m port 22: No route to host\r"),
            "ssh: connect to host m port 22: No route to host"
        );
        assert_eq!(plain("a\tb\x1b[31mc\u{7}"), "a b[31mc");
    }
}
