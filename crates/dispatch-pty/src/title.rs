//! Reading the title a child sets for its terminal.
//!
//! Agents announce what they are doing this way — a title sequence is how a
//! shell says which command is running, and how an agent says which task it is
//! on — and that is what the sidebar should say rather than the harness's name
//! forever.
//!
//! The emulator does not track it: `libghostty-vt` models the screen, and a
//! title is not on the screen. So the byte stream is scanned on its way past,
//! which also means one scanner serves a pane whether its process is local or
//! in the daemon: both hand over the same bytes.

/// Longest title kept, in bytes.
///
/// A title is a line in a sidebar. Without a cap, a child that opens a title
/// sequence and never terminates it would grow this without limit.
const MAX_TITLE: usize = 512;

/// Where the scanner is in a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Ordinary output.
    Text,
    /// Seen `ESC`.
    Escape,
    /// Seen `ESC ]`, reading the command number.
    Command,
    /// Inside a title's text.
    Title,
    /// Inside some other OSC, which is skipped.
    Other,
    /// Seen `ESC` inside an OSC: `ESC \` ends it.
    Terminator,
}

/// Finds the titles a child sets, across however many chunks they arrive in.
///
/// Handles the three sequences that set a title — `OSC 0`, `OSC 1` and `OSC 2`,
/// ended by either `BEL` or `ESC \` — and skips every other OSC, of which the
/// common ones carry hyperlinks and clipboard data that have nothing to do with
/// a name.
#[derive(Debug)]
pub struct TitleScanner {
    state: State,
    /// The command number so far, as digits arrive.
    command: u16,
    /// Whether a command number has been seen at all.
    numbered: bool,
    pending: Vec<u8>,
    /// Set when a title overran [`MAX_TITLE`], so the rest is skipped rather
    /// than a truncated name being reported.
    overran: bool,
}

impl Default for TitleScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl TitleScanner {
    /// Creates a scanner waiting for a sequence.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Text,
            command: 0,
            numbered: false,
            pending: Vec::new(),
            overran: false,
        }
    }

    /// Scans `bytes`, returning the last title completed in them.
    ///
    /// The last rather than every one: a child that sets several titles in one
    /// write means only the newest, and the caller wants what to display.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<String> {
        let mut latest = None;

        for &byte in bytes {
            if let Some(title) = self.step(byte) {
                latest = Some(title);
            }
        }

        latest
    }

    /// Consumes one byte.
    fn step(&mut self, byte: u8) -> Option<String> {
        const ESC: u8 = 0x1b;
        const BEL: u8 = 0x07;

        match self.state {
            State::Text => {
                if byte == ESC {
                    self.state = State::Escape;
                }
            }

            State::Escape => {
                self.state = if byte == b']' {
                    self.command = 0;
                    self.numbered = false;
                    State::Command
                } else {
                    // Any other escape sequence is the emulator's business.
                    State::Text
                };
            }

            State::Command => match byte {
                b'0'..=b'9' => {
                    // Saturating, so a long run of digits cannot wrap into a
                    // number that happens to mean "title".
                    self.command = self
                        .command
                        .saturating_mul(10)
                        .saturating_add(u16::from(byte - b'0'));
                    self.numbered = true;
                }
                b';' => {
                    self.pending.clear();
                    self.overran = false;
                    // `OSC 0` sets both icon name and title, `OSC 1` the icon
                    // name, `OSC 2` the title. All three are what a child means
                    // by "call me this".
                    self.state = if self.numbered && self.command <= 2 {
                        State::Title
                    } else {
                        State::Other
                    };
                }
                BEL => self.state = State::Text,
                ESC => self.state = State::Terminator,
                _ => self.state = State::Other,
            },

            State::Title => match byte {
                BEL => {
                    self.state = State::Text;
                    return self.finish();
                }
                ESC => self.state = State::Terminator,
                // A control byte has no business in a name, and a stray one
                // would otherwise end up in the sidebar.
                0x00..=0x1f | 0x7f => {}
                _ => self.push(byte),
            },

            State::Other => match byte {
                BEL => self.state = State::Text,
                ESC => self.state = State::Terminator,
                _ => {}
            },

            State::Terminator => {
                // `ESC \` ends an OSC. Anything else means the sequence was
                // interrupted, and whatever follows is ordinary output.
                let ended_title = byte == b'\\' && !self.pending.is_empty();
                self.state = State::Text;

                if ended_title {
                    return self.finish();
                }

                self.pending.clear();
            }
        }

        None
    }

    /// Adds one byte of title text.
    fn push(&mut self, byte: u8) {
        if self.overran {
            return;
        }

        if self.pending.len() + 1 > MAX_TITLE {
            self.overran = true;
            self.pending.clear();
            return;
        }

        // Kept as bytes and decoded at the end, so a character split across two
        // writes is still one character.
        self.pending.push(byte);
    }

    /// Takes the finished title, if there is one worth reporting.
    fn finish(&mut self) -> Option<String> {
        let bytes = std::mem::take(&mut self.pending);
        let overran = std::mem::take(&mut self.overran);

        // Lossy: a child that writes a broken byte in its title should get a
        // replacement character in the sidebar, not have the title dropped.
        let title = String::from_utf8_lossy(&bytes).trim().to_string();

        if overran || title.is_empty() {
            return None;
        }

        Some(title)
    }
}

#[cfg(test)]
mod tests;
