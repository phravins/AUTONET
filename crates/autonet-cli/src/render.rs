//! Terminal-output helpers.

use std::io::IsTerminal;

use owo_colors::OwoColorize;

/// Whether to emit ANSI colour, decided once at startup.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    enabled: bool,
}

impl Theme {
    /// Enable color only for interactive terminals.
    pub fn detect() -> Self {
        let forbidden = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
            || std::env::var_os("TERM").is_some_and(|t| t == "dumb");
        Self {
            enabled: std::io::stdout().is_terminal() && !forbidden,
        }
    }

    /// A theme that never emits escape codes, for tests and JSON paths.
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    /// A theme that always emits escape codes.
    ///
    /// Test-only: the point of it is to prove that styling a cell does not
    /// disturb the column it sits in, which cannot be checked against a theme
    /// that paints nothing.
    #[cfg(test)]
    pub fn coloured() -> Self {
        Self { enabled: true }
    }

    /// A field label.
    pub fn label(self, text: &str) -> String {
        self.paint(text, |t| t.dimmed().to_string())
    }

    /// The answer the user came for.
    pub fn value(self, text: &str) -> String {
        self.paint(text, |t| t.bold().cyan().to_string())
    }

    /// Something working as intended.
    pub fn good(self, text: &str) -> String {
        self.paint(text, |t| t.green().to_string())
    }

    /// Something the user should notice but that is not an error.
    pub fn warn(self, text: &str) -> String {
        self.paint(text, |t| t.yellow().to_string())
    }

    /// A failure.
    pub fn bad(self, text: &str) -> String {
        self.paint(text, |t| t.red().to_string())
    }

    /// Secondary detail.
    pub fn muted(self, text: &str) -> String {
        self.paint(text, |t| t.dimmed().to_string())
    }

    /// A heading.
    pub fn heading(self, text: &str) -> String {
        self.paint(text, |t| t.bold().to_string())
    }

    /// Black on white, whatever the terminal's own palette is.
    ///
    /// The one place AutoNet overrides the user's colours instead of working
    /// with them, because the reader is a camera rather than a person: a QR
    /// code only decodes when its dark modules are actually dark, and block
    /// characters inherit whichever way round the terminal happens to be. See
    /// [`crate::qr`].
    pub fn scannable(self, text: &str) -> String {
        self.paint(text, |t| t.black().on_bright_white().to_string())
    }

    /// Whether escape codes are being emitted at all.
    ///
    /// Exposed for [`crate::qr`] alone, which has to choose a block-character
    /// polarity in the case where it cannot choose a colour.
    pub fn is_coloured(self) -> bool {
        self.enabled
    }

    fn paint(self, text: &str, style: impl Fn(&str) -> String) -> String {
        if self.enabled {
            style(text)
        } else {
            text.to_string()
        }
    }
}

/// Render rows as a left-aligned table with two spaces between columns.
///
/// Widths are computed from the content rather than fixed, because interface
/// names range from `lo` to `br-18642d3532b2` and a fixed width either wastes
/// half the terminal or wraps.
///
/// Note that width is measured in `char`s. Interface names are ASCII on every
/// platform AutoNet supports, so this cannot misalign in practice.
pub fn table(headers: &[&str], rows: &[Vec<String>], theme: Theme) -> String {
    let columns = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate().take(columns) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let mut header_line = String::new();
    for (i, header) in headers.iter().enumerate() {
        push_cell(&mut header_line, header, widths[i], i + 1 == columns);
    }
    out.push_str(&theme.label(header_line.trim_end()));
    out.push('\n');

    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate().take(columns) {
            push_cell(&mut line, cell, widths[i], i + 1 == columns);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }

    out
}

fn push_cell(line: &mut String, cell: &str, width: usize, last: bool) {
    line.push_str(cell);
    if !last {
        for _ in 0..=width.saturating_sub(cell.chars().count()) {
            line.push(' ');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_theme_emits_no_escape_codes() {
        let theme = Theme::plain();
        for rendered in [
            theme.value("x"),
            theme.label("x"),
            theme.good("x"),
            theme.bad("x"),
            theme.warn("x"),
            theme.muted("x"),
            theme.heading("x"),
        ] {
            assert_eq!(rendered, "x");
        }
    }

    #[test]
    fn columns_are_sized_to_their_widest_cell() {
        let rows = vec![
            vec!["lo".into(), "up".into()],
            vec!["br-18642d3532b2".into(), "down".into()],
        ];
        let rendered = table(&["NAME", "STATE"], &rows, Theme::plain());
        let lines: Vec<&str> = rendered.lines().collect();

        assert_eq!(lines.len(), 3);
        // Every second column starts at the same offset.
        let offset = lines[0].find("STATE").unwrap();
        assert_eq!(lines[1].find("up"), Some(offset));
        assert_eq!(lines[2].find("down"), Some(offset));
    }

    #[test]
    fn rows_do_not_carry_trailing_whitespace() {
        let rows = vec![vec!["a".into(), "b".into()]];
        for line in table(&["ONE", "TWO"], &rows, Theme::plain()).lines() {
            assert_eq!(line.trim_end(), line, "trailing space in {line:?}");
        }
    }
}
