//! Turning pointer events into the bytes a child expects.
//!
//! Whether an event is reported at all, and in which encoding, depends on the
//! tracking modes the child set: `?1000` for clicks, `?1002` for drags,
//! `?1003` for every motion, and `?1006` for the modern report format.
//! libghostty-vt tracks those, so the encoder is configured from the terminal
//! rather than from guesses about what the child wants.

use std::ffi::c_void;

use crate::keys::Modifiers;
use crate::sys;
use crate::vt::{Size, VtError, VtTerminal};

/// Which pointer button an event concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left.
    Left,
    /// Middle.
    Middle,
    /// Right.
    Right,
    /// Wheel up.
    WheelUp,
    /// Wheel down.
    WheelDown,
    /// Wheel left.
    WheelLeft,
    /// Wheel right.
    WheelRight,
    /// Motion with no button held.
    None,
}

impl MouseButton {
    fn to_ghostty(self) -> i32 {
        match self {
            Self::Left => sys::mouse_button::LEFT,
            Self::Middle => sys::mouse_button::MIDDLE,
            Self::Right => sys::mouse_button::RIGHT,
            Self::WheelUp => sys::mouse_button::FOUR,
            Self::WheelDown => sys::mouse_button::FIVE,
            Self::WheelLeft => sys::mouse_button::SIX,
            Self::WheelRight => sys::mouse_button::SEVEN,
            Self::None => sys::mouse_button::UNKNOWN,
        }
    }
}

/// What the pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// A button went down.
    Press,
    /// A button came up.
    Release,
    /// The pointer moved.
    Motion,
}

impl MouseAction {
    fn to_ghostty(self) -> i32 {
        match self {
            Self::Press => sys::mouse_action::PRESS,
            Self::Release => sys::mouse_action::RELEASE,
            Self::Motion => sys::mouse_action::MOTION,
        }
    }
}

/// One pointer event, in cells relative to the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    /// What happened.
    pub action: MouseAction,
    /// Which button.
    pub button: MouseButton,
    /// Column within the pane, zero-indexed.
    pub col: u16,
    /// Row within the pane, zero-indexed.
    pub row: u16,
    /// Modifiers held.
    pub modifiers: Modifiers,
}

/// Encodes pointer events for one pane.
#[derive(Debug)]
pub struct MouseEncoder {
    encoder: sys::MouseEncoder,
    event: sys::MouseEvent,
}

// SAFETY: both handles are owned exclusively by this value, and every method
// takes &mut self, so the library never sees concurrent use of either.
unsafe impl Send for MouseEncoder {}

impl MouseEncoder {
    /// Creates an encoder.
    pub fn new() -> Result<Self, VtError> {
        let mut encoder: sys::MouseEncoder = std::ptr::null_mut();
        // SAFETY: valid out-pointer; a null allocator selects the default.
        let code = unsafe { sys::ghostty_mouse_encoder_new(std::ptr::null(), &raw mut encoder) };
        check("ghostty_mouse_encoder_new", code)?;

        let mut event: sys::MouseEvent = std::ptr::null_mut();
        // SAFETY: valid out-pointer; a null allocator selects the default.
        let code = unsafe { sys::ghostty_mouse_event_new(std::ptr::null(), &raw mut event) };
        if let Err(error) = check("ghostty_mouse_event_new", code) {
            // SAFETY: created above and not used again.
            unsafe { sys::ghostty_mouse_encoder_free(encoder) };
            return Err(error);
        }

        Ok(Self { encoder, event })
    }

    /// Encodes one pointer event for `terminal`.
    ///
    /// Returns an empty vector when the child is not tracking the mouse, or is
    /// not tracking this kind of event. That is the normal case: most of the
    /// time a pane wants nothing, and sending it anything would print garbage
    /// into whatever it is running.
    pub fn encode(
        &mut self,
        terminal: &VtTerminal,
        size: Size,
        input: MouseInput,
    ) -> Result<Vec<u8>, VtError> {
        // The child can turn tracking on and off at any time, so the encoder
        // follows the terminal rather than being configured once.
        //
        // SAFETY: both handles are live for the duration of the call.
        unsafe {
            sys::ghostty_mouse_encoder_setopt_from_terminal(self.encoder, terminal.handle());
        }

        let encoder_size = sys::MouseEncoderSize::in_cells(size.cols, size.rows);

        // SAFETY: the encoder is live and `encoder_size` is a
        // GhosttyMouseEncoderSize with its `size` field set, which is what
        // this option documents.
        unsafe {
            sys::ghostty_mouse_encoder_setopt(
                self.encoder,
                sys::MOUSE_ENCODER_OPT_SIZE,
                (&raw const encoder_size).cast::<c_void>(),
            );
        }

        // Whether a button is down decides how motion is reported: a drag is
        // reported to a pane tracking button events, while free motion is
        // only wanted by one tracking all motion.
        let any_button_pressed = !matches!(input.button, MouseButton::None);

        // SAFETY: the encoder is live and this option takes a bool.
        unsafe {
            sys::ghostty_mouse_encoder_setopt(
                self.encoder,
                sys::MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED,
                (&raw const any_button_pressed).cast::<c_void>(),
            );
        }

        // Cell coordinates are passed straight through because the declared
        // cell is one pixel square.
        let position = sys::MousePosition {
            x: f32::from(input.col),
            y: f32::from(input.row),
        };

        // SAFETY: `self.event` is live; these setters take plain values.
        unsafe {
            sys::ghostty_mouse_event_set_action(self.event, input.action.to_ghostty());
            sys::ghostty_mouse_event_set_button(self.event, input.button.to_ghostty());
            sys::ghostty_mouse_event_set_mods(self.event, input.modifiers.to_ghostty());
            sys::ghostty_mouse_event_set_position(self.event, position);
        }

        let mut buf = [0u8; 64];
        let mut written: usize = 0;

        // SAFETY: both handles are live and `buf` is described accurately.
        let code = unsafe {
            sys::ghostty_mouse_encoder_encode(
                self.encoder,
                self.event,
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut written,
            )
        };

        if code == sys::OUT_OF_SPACE {
            let mut heap = vec![0u8; written];
            let mut written_again: usize = 0;

            // SAFETY: same contract, with the size the library asked for.
            let code = unsafe {
                sys::ghostty_mouse_encoder_encode(
                    self.encoder,
                    self.event,
                    heap.as_mut_ptr(),
                    heap.len(),
                    &raw mut written_again,
                )
            };
            check("ghostty_mouse_encoder_encode", code)?;
            heap.truncate(written_again);
            return Ok(heap);
        }

        check("ghostty_mouse_encoder_encode", code)?;
        Ok(buf[..written.min(buf.len())].to_vec())
    }
}

impl Drop for MouseEncoder {
    fn drop(&mut self) {
        // SAFETY: both handles came from the library, are freed exactly once
        // here, and are not used afterwards.
        unsafe {
            sys::ghostty_mouse_event_free(self.event);
            sys::ghostty_mouse_encoder_free(self.encoder);
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
mod tests {
    use super::*;

    fn terminal() -> VtTerminal {
        VtTerminal::new(Size::new(80, 24)).expect("a terminal can be created")
    }

    const SIZE: Size = Size { cols: 80, rows: 24 };

    fn click(col: u16, row: u16) -> MouseInput {
        MouseInput {
            action: MouseAction::Press,
            button: MouseButton::Left,
            col,
            row,
            modifiers: Modifiers::NONE,
        }
    }

    fn encode(terminal: &VtTerminal, input: MouseInput) -> Vec<u8> {
        MouseEncoder::new()
            .expect("an encoder can be created")
            .encode(terminal, SIZE, input)
            .expect("encoding succeeds")
    }

    #[test]
    fn a_pane_not_tracking_the_mouse_receives_nothing() {
        // Sending a report to a program that never asked would print garbage
        // into whatever it is running.
        let terminal = terminal();
        assert!(encode(&terminal, click(5, 5)).is_empty());
    }

    #[test]
    fn enabling_click_tracking_produces_a_report() {
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h");

        assert!(
            !encode(&terminal, click(5, 5)).is_empty(),
            "a tracking pane should receive a click"
        );
    }

    #[test]
    fn disabling_tracking_stops_the_reports() {
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h");
        assert!(!encode(&terminal, click(5, 5)).is_empty());

        terminal.feed(b"\x1b[?1000l");
        assert!(
            encode(&terminal, click(5, 5)).is_empty(),
            "reports must stop as soon as the child turns tracking off"
        );
    }

    #[test]
    fn motion_is_not_reported_to_a_pane_that_only_asked_for_clicks() {
        // ?1000 is clicks only. Reporting motion would flood a child that
        // asked for none.
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h");

        let motion = MouseInput {
            action: MouseAction::Motion,
            button: MouseButton::None,
            col: 5,
            row: 5,
            modifiers: Modifiers::NONE,
        };

        assert!(encode(&terminal, motion).is_empty());
    }

    #[test]
    fn a_drag_is_reported_to_a_pane_tracking_button_motion() {
        // Dragging with a button held is what selection inside an agent is
        // made of, so this is the motion case that matters.
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1002h\x1b[?1006h");

        let drag_at = |col| MouseInput {
            action: MouseAction::Motion,
            button: MouseButton::Left,
            col,
            row: 5,
            modifiers: Modifiers::NONE,
        };

        let mut encoder = MouseEncoder::new().expect("creatable");
        let first = encoder
            .encode(&terminal, SIZE, drag_at(5))
            .expect("encoding succeeds");
        let second = encoder
            .encode(&terminal, SIZE, drag_at(6))
            .expect("encoding succeeds");

        assert!(!first.is_empty(), "a drag should be reported");
        assert_ne!(first, second, "each cell crossed should differ");
    }

    #[test]
    fn button_less_motion_is_not_forwarded() {
        // A known limitation rather than a decision: libghostty-vt's encoder
        // produces nothing for motion with no button held, under ?1002 and
        // ?1003 alike, even with ANY_BUTTON_PRESSED set. Hover effects inside
        // an agent will therefore not respond. Asserted so that a future
        // upstream change is noticed here rather than in a pane.
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1003h\x1b[?1006h");

        let mut encoder = MouseEncoder::new().expect("creatable");
        let hover = |col| MouseInput {
            action: MouseAction::Motion,
            button: MouseButton::None,
            col,
            row: 5,
            modifiers: Modifiers::NONE,
        };

        let _ = encoder.encode(&terminal, SIZE, hover(5)).expect("succeeds");
        let moved = encoder.encode(&terminal, SIZE, hover(6)).expect("succeeds");

        assert!(moved.is_empty(), "upstream behaviour has changed");
    }

    #[test]
    fn the_sgr_format_is_used_when_the_child_asks_for_it() {
        // SGR reports start with ESC [ < and are what any modern program wants,
        // because the legacy format cannot address past column 223.
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h\x1b[?1006h");

        let bytes = encode(&terminal, click(5, 5));
        assert!(
            bytes.starts_with(b"\x1b[<"),
            "expected an SGR report, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn the_report_names_the_cell_that_was_clicked() {
        // SGR reports are one-indexed, so a click on cell (4, 9) is "5;10".
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h\x1b[?1006h");

        let bytes = encode(&terminal, click(4, 9));
        let text = String::from_utf8_lossy(&bytes).into_owned();

        assert!(
            text.contains("5;10"),
            "expected the clicked cell in {text:?}"
        );
    }

    #[test]
    fn a_release_differs_from_a_press() {
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h\x1b[?1006h");

        let press = encode(&terminal, click(5, 5));
        let release = encode(
            &terminal,
            MouseInput {
                action: MouseAction::Release,
                ..click(5, 5)
            },
        );

        assert_ne!(press, release, "a release must be distinguishable");
    }

    #[test]
    fn the_wheel_is_reported() {
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h\x1b[?1006h");

        let wheel = MouseInput {
            action: MouseAction::Press,
            button: MouseButton::WheelUp,
            col: 5,
            row: 5,
            modifiers: Modifiers::NONE,
        };

        assert!(!encode(&terminal, wheel).is_empty());
    }

    #[test]
    fn one_encoder_serves_many_events() {
        let mut terminal = terminal();
        terminal.feed(b"\x1b[?1000h\x1b[?1006h");
        let mut encoder = MouseEncoder::new().expect("creatable");

        for col in 0..5 {
            let bytes = encoder
                .encode(&terminal, SIZE, click(col, 0))
                .expect("encoding succeeds");
            assert!(!bytes.is_empty(), "click at column {col} produced nothing");
        }
    }
}
