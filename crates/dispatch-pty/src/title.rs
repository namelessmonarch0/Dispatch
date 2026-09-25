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
    /// Inside a title's or a progress report's text.
    Payload,
    /// Inside some other OSC, which is skipped.
    Other,
    /// Seen `ESC` inside an OSC: `ESC \` ends it.
    Terminator,
}

/// Which kind of payload the scanner is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Collecting {
    /// A title, from `OSC 0`, `OSC 1` or `OSC 2`.
    Title,
    /// A progress report, from `OSC 9;4`.
    Progress,
}

/// What one byte finished.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Title(String),
    Progress(String),
    Bell,
}

/// What a chunk of output said besides its text.
///
/// The title is how an agent names itself and — for several — how it says it
/// is busy; progress (`OSC 9;4`) is how some report a running turn; a bare
/// bell is how a program asks to be looked at. All three are read off the
/// same bytes the screen is drawn from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {
    /// The last title completed in the chunk.
    pub title: Option<String>,
    /// The last `OSC 9;4` payload completed in the chunk, after `9;`: `4;0`,
    /// `4;1;40`, …
    pub progress: Option<String>,
    /// Whether a bare `BEL` rang.
    pub bell: bool,
}

/// The OSC number that carries progress reports, as `9;4;…`.
const NOTIFY: u16 = 9;

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
    /// What the payload being read will be, once it ends.
    collecting: Collecting,
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
            collecting: Collecting::Title,
        }
    }

    /// Scans `bytes`, returning the last title completed in them.
    ///
    /// The last rather than every one: a child that sets several titles in one
    /// write means only the newest, and the caller wants what to display.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<String> {
        self.scan_signals(bytes).title
    }

    /// Scans `bytes` for everything they say besides their text.
    pub fn scan_signals(&mut self, bytes: &[u8]) -> Signals {
        let mut signals = Signals::default();

        for &byte in bytes {
            match self.step(byte) {
                Some(Event::Title(title)) => signals.title = Some(title),
                Some(Event::Progress(progress)) => signals.progress = Some(progress),
                Some(Event::Bell) => signals.bell = true,
                None => {}
            }
        }

        signals
    }

    /// Consumes one byte.
    fn step(&mut self, byte: u8) -> Option<Event> {
        const ESC: u8 = 0x1b;
        const BEL: u8 = 0x07;

        match self.state {
            State::Text => {
                if byte == ESC {
                    self.state = State::Escape;
                } else if byte == BEL {
                    return Some(Event::Bell);
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
                    // by "call me this". `OSC 9` is read too, for the progress
                    // reports that travel as `9;4;…`.
                    self.state = if self.numbered && self.command <= 2 {
                        self.collecting = Collecting::Title;
                        State::Payload
                    } else if self.numbered && self.command == NOTIFY {
                        self.collecting = Collecting::Progress;
                        State::Payload
                    } else {
                        State::Other
                    };
                }
                BEL => self.state = State::Text,
                ESC => self.state = State::Terminator,
                _ => self.state = State::Other,
            },

            State::Payload => match byte {
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
                let ended_payload = byte == b'\\' && !self.pending.is_empty();
                self.state = State::Text;

                if ended_payload {
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

    /// Takes the finished payload, if there is one worth reporting.
    fn finish(&mut self) -> Option<Event> {
        let bytes = std::mem::take(&mut self.pending);
        let overran = std::mem::take(&mut self.overran);

        // Lossy: a child that writes a broken byte in its title should get a
        // replacement character in the sidebar, not have the title dropped.
        let text = String::from_utf8_lossy(&bytes).trim().to_string();

        if overran || text.is_empty() {
            return None;
        }

        match self.collecting {
            Collecting::Title => Some(Event::Title(text)),
            // `OSC 9` alone is a notification; only `9;4` reports progress.
            Collecting::Progress => text.starts_with("4;").then_some(Event::Progress(text)),
        }
    }
}

#[cfg(test)]
mod tests;
