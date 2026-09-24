//! Starting a process in a pseudoterminal.
//!
//! The platform half of a pane. On Unix this is `portable-pty`. On Windows
//! it is Dispatch's own ConPTY spawn: a pane's process has to be created
//! suspended and put in a Job Object before it runs, or anything it starts
//! first escapes the job -- and `portable-pty` starts it running.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

#[cfg(windows)]
mod windows;

/// What to start.
#[derive(Debug, Clone, Copy)]
pub struct PtyCommand<'a> {
    /// The program, found on `PATH` when not a path itself.
    pub program: &'a str,
    /// Its arguments.
    pub args: &'a [String],
    /// Variables set on top of the environment this process has.
    pub env: &'a BTreeMap<String, String>,
    /// Where it starts.
    pub cwd: &'a Path,
}

/// A process running in a pseudoterminal.
pub struct PtyProcess {
    /// What the process prints.
    pub reader: Box<dyn Read + Send>,
    /// What it reads, as if typed.
    pub writer: Box<dyn Write + Send>,
    /// The pseudoterminal. Held for as long as the process should have one.
    pub terminal: Terminal,
    /// The process, for waiting on.
    pub child: Child,
    /// Its process id, which `process::terminate_tree` ends the tree by.
    pub pid: Option<u32>,
}

impl std::fmt::Debug for PtyProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyProcess")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// A pseudoterminal. Dropping it ends it.
pub struct Terminal(imp::Terminal);

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Terminal")
    }
}

impl Terminal {
    /// Resizes the pseudoterminal; the process learns of it as a terminal
    /// resize.
    pub fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        self.0.resize(rows, cols)
    }
}

/// A process to wait on.
pub struct Child(imp::Child);

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Child")
    }
}

impl Child {
    /// Waits for the process to exit.
    ///
    /// 0 for success, the process's code otherwise, and 1 for a failure
    /// that carried no code: a failed exit must never read as a clean one.
    #[must_use]
    pub fn wait(self) -> i32 {
        self.0.wait()
    }
}

/// Starts `command` in a new pseudoterminal of `rows` by `cols`.
pub fn spawn(command: &PtyCommand<'_>, rows: u16, cols: u16) -> std::io::Result<PtyProcess> {
    imp::spawn(command, rows, cols)
}

/// `arg` quoted so the C runtime's command-line parser splits it back out
/// unchanged.
///
/// Left alone when it has no space, tab, newline, vertical tab or quote;
/// otherwise wrapped in quotes, with each quote escaped and the backslashes
/// before a quote -- or before the closing quote -- doubled. The rules
/// `portable-pty` 0.9 followed, so every argument reaches a program as it
/// did. They are not `cmd.exe`'s rules: nothing that has to reach a program
/// intact can travel as an argument that `cmd.exe` parses again.
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows spawn builds a command line; the tests run everywhere"
    )
)]
fn quote_for_crt(arg: &str) -> String {
    let plain = !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']);
    if plain {
        return arg.to_string();
    }

    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        let doubled = if c == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        quoted.extend(std::iter::repeat_n('\\', doubled));
        backslashes = 0;
        quoted.push(c);
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(unix)]
mod imp {
    use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

    pub(super) struct Terminal(Box<dyn MasterPty + Send>);

    impl Terminal {
        pub(super) fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
            self.0
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| std::io::Error::other(format!("{error:#}")))
        }
    }

    pub(super) struct Child(Box<dyn portable_pty::Child + Send + Sync>);

    impl Child {
        pub(super) fn wait(mut self) -> i32 {
            match self.0.wait() {
                Ok(status) if status.success() => 0,
                Ok(status) => i32::try_from(status.exit_code()).unwrap_or(1),
                Err(_) => 1,
            }
        }
    }

    pub(super) fn spawn(
        command: &super::PtyCommand<'_>,
        rows: u16,
        cols: u16,
    ) -> std::io::Result<super::PtyProcess> {
        // `portable-pty` reports through `anyhow`; this crate speaks
        // `io::Error`, with the whole chain kept in the message.
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| {
                std::io::Error::other(format!("failed to open a pseudoterminal: {e:#}"))
            })?;

        let mut builder = CommandBuilder::new(command.program);
        builder.args(command.args);
        builder.cwd(command.cwd);
        for (key, value) in command.env {
            builder.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;

        // The slave is held open by the child, and dropping this copy is what
        // lets the reader see end-of-file when the child exits.
        drop(pair.slave);

        let pid = child.process_id();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;

        Ok(super::PtyProcess {
            reader,
            writer,
            terminal: super::Terminal(Terminal(pair.master)),
            child: super::Child(Child(child)),
            pid,
        })
    }
}

#[cfg(windows)]
use self::windows as imp;

#[cfg(test)]
mod tests {
    use super::quote_for_crt;

    #[test]
    fn a_plain_argument_is_left_alone() {
        assert_eq!(quote_for_crt("claude"), "claude");
        assert_eq!(
            quote_for_crt("<%DISPATCH_TASK_FILE%"),
            "<%DISPATCH_TASK_FILE%"
        );
    }

    #[test]
    fn an_argument_with_a_space_is_quoted() {
        assert_eq!(quote_for_crt("two words"), "\"two words\"");
        assert_eq!(quote_for_crt(""), "\"\"");
    }

    #[test]
    fn quotes_and_the_backslashes_before_them_are_escaped() {
        assert_eq!(quote_for_crt(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_for_crt(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_for_crt(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_for_crt(r"C:\a\b c"), r#""C:\a\b c""#);
    }
}
