//! `dispatch-pty`: pseudoterminal supervision and terminal emulation.

pub mod keys;
pub mod mouse;
pub mod screen;
pub mod session;
pub mod sys;
pub mod title;
pub mod vt;

pub use keys::{Key, KeyEncoder, Modifiers};
pub use mouse::{MouseAction, MouseButton, MouseEncoder, MouseInput};
pub use screen::{Attrs, Cell, Rgb, Screen, ScreenReader};
pub use session::{Pty, PtyError, PtySession, RunState};
pub use title::TitleScanner;
pub use vt::{Cursor, ScrollTo, Size, VtError, VtTerminal, encode_paste};
