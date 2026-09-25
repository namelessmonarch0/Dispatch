//! Colours for Dispatch's own chrome, mixed from the terminal's.
//!
//! Dispatch draws around the agents rather than over them, so its colours
//! should sit with whatever theme the terminal already has. It asks the
//! terminal for its background, foreground and one accent at startup and
//! mixes everything else from those three; a terminal that does not answer
//! gets a built-in dark palette in the same spirit.

use ratatui::style::Color;

/// A colour as three 8-bit channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `self` moved `amount` of the way toward `other`, from 0.0 to 1.0.
    #[must_use]
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let channel = |from: u8, to: u8| {
            let from = f32::from(from);
            let to = f32::from(to);
            (from + (to - from) * amount).round().clamp(0.0, 255.0) as u8
        };

        Rgb(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }
}

/// The three colours everything else is mixed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The terminal's background.
    pub background: Rgb,
    /// The terminal's default text colour.
    pub foreground: Rgb,
    /// Palette slot 5, magenta in most themes: the one colour Dispatch
    /// uses to say "this has the keyboard".
    pub accent: Rgb,
}

impl Palette {
    /// The built-in palette, for a terminal that does not say what its own is.
    pub const FALLBACK: Palette = Palette {
        background: Rgb(0x16, 0x16, 0x1e),
        foreground: Rgb(0xc8, 0xc8, 0xd8),
        accent: Rgb(0xb4, 0xa0, 0xf0),
    };
}

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Any 24-bit colour.
    TrueColor,
    /// The xterm 256-colour palette.
    ///
    /// The safe assumption: a terminal without 24-bit colour misreads an RGB
    /// escape rather than approximating it.
    Indexed,
}

impl Depth {
    /// Read from `COLORTERM`, the one variable terminals use to say so.
    #[must_use]
    pub fn from_colorterm(value: Option<&str>) -> Depth {
        match value {
            Some("truecolor" | "24bit") => Depth::TrueColor,
            _ => Depth::Indexed,
        }
    }
}

/// What Dispatch's chrome is drawn in.
///
/// Ordinary text is not here: it stays the terminal's own foreground, and only
/// what is written over `tint` or `tab` takes `text`. Nor are the state
/// glyphs' colours, which are ANSI and so already the theme's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Secondary text: branches, the app name, inactive tabs, unfocused
    /// borders, the status row.
    pub faded: Color,
    /// The row behind a selected project or a focused pane.
    pub tint: Color,
    /// The active tab's background.
    pub tab: Color,
    /// The focused pane's border.
    pub accent: Color,
    /// Text drawn over `tint` or `tab`: the palette's own foreground.
    ///
    /// Those two are mixed from the palette, so what is written on them comes
    /// from it too. When the terminal answered, this is its own text colour;
    /// when it did not, the tint is the fallback's dark one, and the
    /// terminal's text on it could be a light theme's dark text.
    pub text: Color,
    /// What everything above was mixed from, kept so an animation can blend
    /// between roles at the theme's own depth rather than carrying the
    /// palette and depth around separately.
    palette: Palette,
    depth: Depth,
}

impl Theme {
    /// Mixes a theme from `palette`, at the depth the terminal can show.
    #[must_use]
    pub fn new(palette: Palette, depth: Depth) -> Theme {
        let colour = |rgb: Rgb| match depth {
            Depth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            Depth::Indexed => Color::Indexed(nearest_indexed(rgb)),
        };

        // The accent is palette slot 5, so in 256 colours it is named by its
        // slot rather than approximated: the terminal draws its own colour
        // exactly, where the nearest cube entry is only close to it.
        let accent = match depth {
            Depth::TrueColor => colour(palette.accent),
            Depth::Indexed => Color::Indexed(5),
        };

        Theme {
            faded: colour(palette.foreground.mix(palette.background, 0.45)),
            tint: colour(palette.background.mix(palette.foreground, 0.10)),
            tab: colour(palette.background.mix(palette.accent, 0.30)),
            accent,
            text: colour(palette.foreground),
            palette,
            depth,
        }
    }

    /// The built-in palette at full depth.
    ///
    /// What every test draws with, so no assertion depends on the terminal
    /// running it.
    #[must_use]
    pub fn fallback() -> Theme {
        Theme::new(Palette::FALLBACK, Depth::TrueColor)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::fallback()
    }
}

/// A colour the theme is mixed from, for animations that move between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The terminal's background.
    Background,
    /// Secondary text and unfocused borders.
    Faded,
    /// A selected or focused row.
    Tint,
    /// The active tab.
    Tab,
    /// The focused pane's border.
    Accent,
    /// The peak of an attention pulse: the background most of the way to the
    /// accent.
    Pulse,
}

impl Theme {
    /// The 24-bit colour behind `role`, before it is drawn at the theme's
    /// depth.
    #[must_use]
    pub fn rgb(&self, role: Role) -> Rgb {
        let p = self.palette;
        match role {
            Role::Background => p.background,
            Role::Faded => p.foreground.mix(p.background, 0.45),
            Role::Tint => p.background.mix(p.foreground, 0.10),
            Role::Tab => p.background.mix(p.accent, 0.30),
            Role::Accent => p.accent,
            Role::Pulse => p.background.mix(p.accent, 0.55),
        }
    }

    /// `from` moved `t` of the way toward `to`, drawn at this theme's depth —
    /// so a 256-colour terminal steps through the nearest entries rather than
    /// being sent colours it would misread.
    #[must_use]
    pub fn blend(&self, from: Rgb, to: Rgb, t: f32) -> Color {
        let mixed = from.mix(to, t.clamp(0.0, 1.0));
        match self.depth {
            Depth::TrueColor => Color::Rgb(mixed.0, mixed.1, mixed.2),
            Depth::Indexed => Color::Indexed(nearest_indexed(mixed)),
        }
    }
}

/// The xterm-256 entry nearest `rgb`, from the colour cube or the grey ramp.
///
/// The first sixteen entries are left out: they are the terminal's own theme
/// colours, which are exactly what cannot be known here.
#[must_use]
pub fn nearest_indexed(rgb: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    let level = |channel: u8| {
        (0..LEVELS.len())
            .min_by_key(|&index| (i32::from(LEVELS[index]) - i32::from(channel)).abs())
            .unwrap_or(0)
    };
    let (r, g, b) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube = Rgb(LEVELS[r], LEVELS[g], LEVELS[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;

    // The ramp runs 8, 18, …, 238 in 24 steps.
    let average = (u32::from(rgb.0) + u32::from(rgb.1) + u32::from(rgb.2)) / 3;
    let step = ((average.saturating_sub(8) + 5) / 10).min(23);
    let grey_value = u8::try_from(8 + 10 * step).unwrap_or(u8::MAX);
    let grey = Rgb(grey_value, grey_value, grey_value);
    let grey_index = 232 + step as usize;

    let index = if distance(rgb, grey) < distance(rgb, cube) {
        grey_index
    } else {
        cube_index
    };
    u8::try_from(index).unwrap_or(u8::MAX)
}

/// Squared distance between two colours.
fn distance(a: Rgb, b: Rgb) -> u32 {
    let d = |x: u8, y: u8| {
        let delta = i32::from(x) - i32::from(y);
        delta.unsigned_abs() * delta.unsigned_abs()
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// What is asked of the terminal at startup, as one write.
///
/// Foreground, background and palette slot 5, then primary device
/// attributes. Every terminal answers the last, and answers in order, so its
/// reply arriving means every colour reply that is coming has come.
pub const QUERY: &[u8] = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]4;5;?\x07\x1b[c";

/// What the terminal has answered so far.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Replies {
    /// The default text colour, when it said.
    pub foreground: Option<Rgb>,
    /// The background, when it said.
    pub background: Option<Rgb>,
    /// Palette slot 5, when it said.
    pub accent: Option<Rgb>,
    /// Whether the device-attributes reply has arrived, which ends the wait.
    pub done: bool,
}

impl Replies {
    /// Reads every complete reply in `bytes`.
    ///
    /// Given the whole of what has been read each time rather than the last
    /// chunk: a reply can be split across reads, and re-reading a few dozen
    /// bytes is simpler than carrying half a reply over. An incomplete
    /// sequence at the end is left for the next call.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Replies {
        let mut replies = Replies::default();
        let mut index = 0;

        while index + 1 < bytes.len() {
            if bytes[index] != 0x1b {
                index += 1;
                continue;
            }

            match bytes[index + 1] {
                b']' => {
                    let start = index + 2;
                    let Some((end, next)) = osc_end(bytes, start) else {
                        break;
                    };
                    replies.take_colour(&bytes[start..end]);
                    index = next;
                }
                b'[' => {
                    // Parameters, then one final byte from 0x40 to 0x7e.
                    let start = index + 2;
                    let Some(length) = bytes[start..]
                        .iter()
                        .position(|byte| (0x40..=0x7e).contains(byte))
                    else {
                        break;
                    };
                    let end = start + length;
                    if bytes[end] == b'c' && bytes.get(start) == Some(&b'?') {
                        replies.done = true;
                    }
                    index = end + 1;
                }
                _ => index += 1,
            }
        }

        replies
    }

    /// The palette these replies describe, with the fallback's colour
    /// wherever the terminal said nothing.
    #[must_use]
    pub fn palette(&self) -> Palette {
        Palette {
            background: self.background.unwrap_or(Palette::FALLBACK.background),
            foreground: self.foreground.unwrap_or(Palette::FALLBACK.foreground),
            accent: self.accent.unwrap_or(Palette::FALLBACK.accent),
        }
    }

    /// Records a colour reply's body: `10;rgb:…`, `11;rgb:…` or `4;5;rgb:…`.
    fn take_colour(&mut self, body: &[u8]) {
        let Ok(body) = std::str::from_utf8(body) else {
            return;
        };

        if let Some(value) = body.strip_prefix("10;") {
            self.foreground = parse_rgb(value).or(self.foreground);
        } else if let Some(value) = body.strip_prefix("11;") {
            self.background = parse_rgb(value).or(self.background);
        } else if let Some(value) = body.strip_prefix("4;5;") {
            self.accent = parse_rgb(value).or(self.accent);
        }
    }
}

/// Where an operating-system command starting at `start` ends: the index of
/// its terminator, and the index just past it. `BEL` and `ESC \` both end one.
fn osc_end(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut index = start;

    while index < bytes.len() {
        match bytes[index] {
            0x07 => return Some((index, index + 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, index + 2)),
            _ => index += 1,
        }
    }

    None
}

/// `rgb:R/G/B` or `rgba:R/G/B/A`, each channel one to four hex digits,
/// scaled to eight bits.
fn parse_rgb(value: &str) -> Option<Rgb> {
    let channels = value
        .strip_prefix("rgb:")
        .or_else(|| value.strip_prefix("rgba:"))?;
    let mut parts = channels.split('/');

    let mut channel = || -> Option<u8> {
        let hex = parts.next()?;
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let raw = u32::from_str_radix(hex, 16).ok()?;
        let max = (1_u32 << (4 * hex.len())) - 1;
        u8::try_from((raw * 255 + max / 2) / max).ok()
    };

    Some(Rgb(channel()?, channel()?, channel()?))
}

#[cfg(test)]
mod tests;
