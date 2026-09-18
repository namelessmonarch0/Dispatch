//! Turning key presses into the bytes a child expects.
//!
//! The hard part is not naming keys, it is knowing which encoding the child
//! currently wants: the kitty keyboard protocol when it has negotiated one,
//! `modifyOtherKeys` when it has asked for that, application cursor keys when
//! it is in that mode, and plain escape sequences otherwise. libghostty-vt
//! tracks all of it, so the encoder is configured from the terminal itself
//! rather than from assumptions.

use crate::sys;
use crate::vt::{VtError, VtTerminal};

/// A physical key, named the way libghostty-vt names them.
///
/// Deliberately small: only what a terminal front end can actually report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character key, identified by the character it produces
    /// unmodified.
    Char(char),
    /// Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Escape.
    Escape,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// A function key, one-indexed.
    Function(u8),
}

impl Key {
    /// The `GhosttyKey` value for this key.
    ///
    /// Letters and digits are contiguous in the enum, so they are offset from
    /// their first member rather than listed out.
    fn to_ghostty(self) -> i32 {
        match self {
            Self::Char(c) => match c {
                'a'..='z' => sys::key::A + (c as i32 - 'a' as i32),
                'A'..='Z' => sys::key::A + (c as i32 - 'A' as i32),
                '0'..='9' => sys::key::DIGIT_0 + (c as i32 - '0' as i32),
                ' ' => sys::key::SPACE,
                '`' | '~' => sys::key::BACKQUOTE,
                '\\' | '|' => sys::key::BACKSLASH,
                '[' | '{' => sys::key::BRACKET_LEFT,
                ']' | '}' => sys::key::BRACKET_RIGHT,
                ',' | '<' => sys::key::COMMA,
                '.' | '>' => sys::key::PERIOD,
                '/' | '?' => sys::key::SLASH,
                ';' | ':' => sys::key::SEMICOLON,
                '\'' | '"' => sys::key::QUOTE,
                '-' | '_' => sys::key::MINUS,
                '=' | '+' => sys::key::EQUAL,
                // Anything else is text rather than a known physical key. The
                // encoder still receives its UTF-8, which is what gets sent.
                _ => sys::key::UNIDENTIFIED,
            },
            Self::Enter => sys::key::ENTER,
            Self::Tab => sys::key::TAB,
            Self::Backspace => sys::key::BACKSPACE,
            Self::Escape => sys::key::ESCAPE,
            Self::Delete => sys::key::DELETE,
            Self::Insert => sys::key::INSERT,
            Self::Home => sys::key::HOME,
            Self::End => sys::key::END,
            Self::PageUp => sys::key::PAGE_UP,
            Self::PageDown => sys::key::PAGE_DOWN,
            Self::Up => sys::key::ARROW_UP,
            Self::Down => sys::key::ARROW_DOWN,
            Self::Left => sys::key::ARROW_LEFT,
            Self::Right => sys::key::ARROW_RIGHT,
            // F1 through F24 are contiguous.
            Self::Function(n) if (1..=24).contains(&n) => sys::key::F1 + i32::from(n) - 1,
            Self::Function(_) => sys::key::UNIDENTIFIED,
        }
    }

    /// The text this key produces on its own, if any.
    fn text(self) -> Option<char> {
        match self {
            Self::Char(c) => Some(c),
            _ => None,
        }
    }
}

/// Which modifiers were held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt or Option.
    pub alt: bool,
    /// Super, Command or Windows.
    pub super_: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        super_: false,
    };

    pub(crate) fn to_ghostty(self) -> u16 {
        let mut bits = 0;
        if self.shift {
            bits |= sys::mods::SHIFT;
        }
        if self.ctrl {
            bits |= sys::mods::CTRL;
        }
        if self.alt {
            bits |= sys::mods::ALT;
        }
        if self.super_ {
            bits |= sys::mods::SUPER;
        }
        bits
    }
}

/// Encodes key presses for one pane.
///
/// Reconfigured from the terminal on every encode, because the child can
/// change keyboard mode at any moment and the next keystroke has to be encoded
/// the way it now expects.
#[derive(Debug)]
pub struct KeyEncoder {
    encoder: sys::KeyEncoder,
    event: sys::KeyEvent,
}

// SAFETY: both handles are owned exclusively by this value, and every method
// takes &mut self, so the library never sees concurrent use of either.
unsafe impl Send for KeyEncoder {}

impl KeyEncoder {
    /// Creates an encoder.
    pub fn new() -> Result<Self, VtError> {
        let mut encoder: sys::KeyEncoder = std::ptr::null_mut();
        // SAFETY: valid out-pointer; a null allocator selects the default.
        let code = unsafe { sys::ghostty_key_encoder_new(std::ptr::null(), &raw mut encoder) };
        check("ghostty_key_encoder_new", code)?;

        let mut event: sys::KeyEvent = std::ptr::null_mut();
        // SAFETY: valid out-pointer; a null allocator selects the default.
        let code = unsafe { sys::ghostty_key_event_new(std::ptr::null(), &raw mut event) };
        if let Err(error) = check("ghostty_key_event_new", code) {
            // SAFETY: created above and not used again.
            unsafe { sys::ghostty_key_encoder_free(encoder) };
            return Err(error);
        }

        Ok(Self { encoder, event })
    }

    /// Encodes one key press for `terminal`.
    ///
    /// Returns the bytes to write to the pseudoterminal, which may be empty
    /// when the key produces nothing in the child's current mode.
    pub fn encode(
        &mut self,
        terminal: &VtTerminal,
        key: Key,
        mods: Modifiers,
    ) -> Result<Vec<u8>, VtError> {
        // The child can switch keyboard protocol at any time, so the encoder
        // is re-synced rather than configured once.
        //
        // SAFETY: both handles are live for the duration of the call.
        unsafe {
            sys::ghostty_key_encoder_setopt_from_terminal(self.encoder, terminal.handle());
        }

        // Option-as-alt defaults to off, which makes the encoder discard Alt
        // entirely. That default belongs to a terminal deciding what the
        // macOS Option key means; by the time a keystroke reaches Dispatch the
        // host terminal has already decided, and an Alt modifier here means
        // Alt was pressed. It is not terminal state, so setopt_from_terminal
        // does not set it and it is applied on every encode.
        let option_as_alt = sys::OPTION_AS_ALT_TRUE;

        // SAFETY: the encoder is live and `option_as_alt` is a GhosttyOptionAsAlt
        // value, which is what this option documents.
        unsafe {
            sys::ghostty_key_encoder_setopt(
                self.encoder,
                sys::KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT,
                (&raw const option_as_alt).cast::<std::ffi::c_void>(),
            );
        }

        // SAFETY: `self.event` is live; these setters take plain values.
        unsafe {
            sys::ghostty_key_event_set_action(self.event, sys::KEY_ACTION_PRESS);
            sys::ghostty_key_event_set_key(self.event, key.to_ghostty());
            sys::ghostty_key_event_set_mods(self.event, mods.to_ghostty());
        }

        // The text the key produces, which is what the encoder sends when no
        // protocol demands something more elaborate. A buffer outliving the
        // call is required because the library borrows it.
        //
        let mut utf8 = [0u8; 4];
        let text: &[u8] = match key.text() {
            Some(c) => c.encode_utf8(&mut utf8).as_bytes(),
            None => &[],
        };

        // SAFETY: `text` points at `utf8`, which outlives this call, and its
        // length is its true byte length. An empty slice is passed as a null
        // pointer with zero length, which the library documents as "no text".
        unsafe {
            if text.is_empty() {
                sys::ghostty_key_event_set_utf8(self.event, std::ptr::null(), 0);
            } else {
                sys::ghostty_key_event_set_utf8(self.event, text.as_ptr(), text.len());
            }

            // The unshifted codepoint identifies the key itself, independent
            // of shift, and the kitty protocol reports it whether or not a
            // modifier claimed the keystroke.
            let codepoint = key
                .text()
                .map_or(0, |c| c.to_lowercase().next().unwrap_or(c) as u32);
            sys::ghostty_key_event_set_unshifted_codepoint(self.event, codepoint);
        }

        let mut buf = [0u8; 64];
        let mut written: usize = 0;

        // SAFETY: both handles are live and `buf` is described accurately.
        let code = unsafe {
            sys::ghostty_key_encoder_encode(
                self.encoder,
                self.event,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut written,
            )
        };

        if code == sys::OUT_OF_SPACE {
            // `written` holds the size required. A sequence longer than the
            // inline buffer is rare but has to work.
            let mut heap = vec![0u8; written];
            let mut written_again: usize = 0;

            // SAFETY: same contract, with a buffer of the size the library
            // just asked for.
            let code = unsafe {
                sys::ghostty_key_encoder_encode(
                    self.encoder,
                    self.event,
                    heap.as_mut_ptr(),
                    heap.len(),
                    &raw mut written_again,
                )
            };
            check("ghostty_key_encoder_encode", code)?;
            heap.truncate(written_again);
            return Ok(heap);
        }

        check("ghostty_key_encoder_encode", code)?;
        Ok(buf[..written.min(buf.len())].to_vec())
    }
}

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        // SAFETY: both handles came from the library, are freed exactly once
        // here, and are not used afterwards.
        unsafe {
            sys::ghostty_key_event_free(self.event);
            sys::ghostty_key_encoder_free(self.encoder);
        }
    }
}

fn check(operation: &'static str, code: sys::GhosttyResult) -> Result<(), VtError> {
    if code == sys::SUCCESS {
        Ok(())
    } else {
        Err(VtError { operation, code })
    }
}

#[cfg(test)]
mod tests;
