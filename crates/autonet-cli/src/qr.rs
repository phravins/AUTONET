//! Render a URL as a QR code made of terminal block characters.
//!
//! The last step of the tool's own pitch. `autonet status` can tell you the URL
//! a phone should open, and until now the only way to get it into the phone was
//! to read it off one screen and type it into another. A camera does that in a
//! second, and does not transpose digits.
//!
//! # Polarity, which is the whole difficulty
//!
//! A QR code is not art; it is an image a camera has to decode, and it only
//! decodes when the dark modules are actually darker than the light ones.
//! Half-block characters do not carry a colour of their own — `█` is whatever
//! the terminal's foreground is, and a space is whatever its background is. So
//! the naive rendering is correct on a light terminal and *inverted* on a dark
//! one, which is the theme most developers use. Some scanners cope with an
//! inverted code; not all do, and the ones that fail do so silently.
//!
//! So when AutoNet is emitting colour at all, it paints the code black on white
//! explicitly and stops depending on the user's palette. This is the one place
//! in the tool that overrides the terminal's own colours rather than working
//! with them, and the reason is that the reader here is a camera, not a person.
//!
//! When colour is off — `NO_COLOR`, `TERM=dumb`, or output being piped — there
//! is no way to force it, so the code falls back to the block form oriented for
//! a dark terminal, and `--qr`'s help says what to do if that is wrong.

use std::fmt::Write as _;

use qrcode::render::unicode::Dense1x2;
use qrcode::{EcLevel, QrCode};

use crate::render::Theme;
use crate::CliError;

/// Render `url` as a scannable block-character QR code, newline-terminated.
///
/// # Errors
///
/// Returns [`CliError::Usage`] if the URL will not fit in a QR code, which in
/// practice means it is longer than about 2 KB. Reported rather than unwrapped
/// because `output.default_port` and `--port` are user input, and a panic is
/// not a diagnosis.
pub(crate) fn render(url: &str, theme: Theme) -> Result<String, CliError> {
    // `M` corrects roughly 15% of the code. `L` would make the block smaller,
    // which matters on an 80-column terminal, but a QR code on a screen gets
    // photographed at an angle with glare on it, and recovering from that is
    // exactly what the redundancy is for.
    let code = QrCode::with_error_correction_level(url, EcLevel::M)
        .map_err(|e| CliError::Usage(format!("could not encode {url} as a QR code: {e}")))?;

    let mut render = code.render::<Dense1x2>();
    render.quiet_zone(true);

    if theme.is_coloured() {
        // Default colours: a dark module becomes a block character, drawn in
        // the foreground colour, and a light module becomes a space showing the
        // background. Painting that pair black-on-white gives dark modules that
        // are genuinely dark, on any terminal theme.
        Ok(paint(&render.build(), theme))
    } else {
        // No colour to force, so the block characters have to carry the
        // polarity themselves, and they can only suit one theme. Dark is the
        // one to suit: a dark module becomes a space (the terminal's dark
        // background) and a light module becomes a lit block.
        render.dark_color(Dense1x2::Light);
        render.light_color(Dense1x2::Dark);
        Ok(format!("{}\n", render.build()))
    }
}

/// Paint each row black-on-white, one row at a time.
///
/// Per row rather than around the whole block: a single escape pair wrapping
/// every line would leave the newlines inside the painted region, and terminals
/// differ on whether the background then runs to the right-hand edge. Ending
/// the colour before each newline keeps the code a clean rectangle.
fn paint(image: &str, theme: Theme) -> String {
    let mut out = String::new();
    for line in image.lines() {
        out.push_str(&theme.scannable(line));
        out.push('\n');
    }
    out
}

/// How the code is introduced, and what to do when it will not scan.
///
/// Worth stating in the output rather than only in `--help`: someone whose
/// terminal is light and whose `NO_COLOR` is set will be looking at an inverted
/// code, and the fix is one variable away.
pub(crate) fn caption(url: &str, theme: Theme) -> String {
    let mut out = format!("  {}  {}\n", theme.label("Scan     "), theme.value(url));
    if !theme.is_coloured() {
        let _ = writeln!(
            out,
            "  {}",
            theme.muted(
                "Rendered for a dark terminal. On a light one, unset NO_COLOR \
                 so AutoNet can set the contrast itself."
            )
        );
    }
    out
}
