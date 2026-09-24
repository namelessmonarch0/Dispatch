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
///
/// A command with a NUL anywhere in it is refused with
/// [`std::io::ErrorKind::InvalidInput`] before anything starts.
pub fn spawn(command: &PtyCommand<'_>, rows: u16, cols: u16) -> std::io::Result<PtyProcess> {
    refuse_nul(command)?;
    imp::spawn(command, rows, cols)
}

/// Refuses a command with a NUL anywhere in it.
///
/// Everything reaches the system as NUL-terminated strings, so a NUL inside
/// one ends it early: on Windows, an argument would lose its tail and every
/// argument after it, and an environment variable every one after it,
/// without a word. `std::process::Command` refuses such a command, and
/// `portable-pty` refused a NUL in an argument; this is that refusal, made on
/// every platform before anything is started, saying which part held it.
fn refuse_nul(command: &PtyCommand<'_>) -> std::io::Result<()> {
    let refuse = |part: String| {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the {part} holds a NUL, which would cut it short"),
        ))
    };

    if command.program.contains('\0') {
        return refuse("program".into());
    }
    if let Some(n) = command.args.iter().position(|arg| arg.contains('\0')) {
        return refuse(format!("argument {}", n + 1));
    }
    if let Some((key, _)) = command
        .env
        .iter()
        .find(|(key, value)| key.contains('\0') || value.contains('\0'))
    {
        return refuse(format!("environment variable {key:?}"));
    }
    if command.cwd.as_os_str().as_encoded_bytes().contains(&0) {
        return refuse("working directory".into());
    }
    Ok(())
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

/// `path` when it names a file, or else `path` with the first extension in
/// `pathext` -- a `;`-separated list, as the variable is -- that makes it
/// name one.
///
/// How Windows finds a program named without its extension: `claude` is run
/// as `claude.exe`, or as `claude.cmd` when that is what an installer left.
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows spawn looks for its program; the tests run everywhere"
    )
)]
fn find_with_pathext(path: &Path, pathext: &str) -> Option<std::path::PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    pathext
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| {
            let mut candidate = path.as_os_str().to_owned();
            candidate.push(extension);
            std::path::PathBuf::from(candidate)
        })
        .find(|candidate| candidate.is_file())
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
    use std::collections::BTreeMap;
    use std::io::ErrorKind;

    use super::{PtyCommand, find_with_pathext, quote_for_crt, refuse_nul, spawn};

    /// A fresh directory holding an empty file for each of `names`.
    fn a_dir_with(label: &str, names: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dispatch-os-pathext-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        for name in names {
            std::fs::write(dir.join(name), b"").expect("temp dir is writable");
        }
        dir
    }

    #[test]
    fn a_program_path_is_completed_with_the_first_pathext_that_names_a_file() {
        const PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";
        let dir = a_dir_with(
            "complete",
            &["claude.CMD", "tool.EXE", "tool.COM", "exact", "exact.EXE"],
        );
        std::fs::create_dir_all(dir.join("folder")).expect("temp dir is writable");
        std::fs::write(dir.join("folder.EXE"), b"").expect("temp dir is writable");

        assert_eq!(
            find_with_pathext(&dir.join("claude"), PATHEXT),
            Some(dir.join("claude.CMD")),
            "completed with the extension that names a file"
        );
        assert_eq!(
            find_with_pathext(&dir.join("tool"), PATHEXT),
            Some(dir.join("tool.COM")),
            "in PATHEXT's order"
        );
        assert_eq!(
            find_with_pathext(&dir.join("exact"), PATHEXT),
            Some(dir.join("exact")),
            "a file named exactly is taken as it is"
        );
        assert_eq!(
            find_with_pathext(&dir.join("folder"), PATHEXT),
            Some(dir.join("folder.EXE")),
            "a directory is not a program"
        );
        assert_eq!(find_with_pathext(&dir.join("missing"), PATHEXT), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_nul_anywhere_in_a_command_is_refused() {
        let args = vec!["-c".to_string(), "echo hi".to_string()];
        let env = BTreeMap::from([("KEY".to_string(), "value".to_string())]);
        let cwd = std::env::temp_dir();
        let clean = PtyCommand {
            program: "sh",
            args: &args,
            env: &env,
            cwd: &cwd,
        };
        refuse_nul(&clean).expect("a command with no NUL in it is let through");

        let refused = |command: PtyCommand<'_>, part: &str| {
            let error = refuse_nul(&command).expect_err(part);
            assert_eq!(error.kind(), ErrorKind::InvalidInput, "{part}: {error}");
            assert!(
                error.to_string().contains(part),
                "{error:?} names no {part}"
            );
        };

        refused(
            PtyCommand {
                program: "s\0h",
                ..clean
            },
            "program",
        );
        let cut = vec!["-c".to_string(), "echo hi\0; echo more".to_string()];
        refused(
            PtyCommand {
                args: &cut,
                ..clean
            },
            "argument",
        );
        let key = BTreeMap::from([("K\0EY".to_string(), "value".to_string())]);
        refused(PtyCommand { env: &key, ..clean }, "environment");
        let value = BTreeMap::from([("KEY".to_string(), "val\0ue".to_string())]);
        refused(
            PtyCommand {
                env: &value,
                ..clean
            },
            "environment",
        );
        let dir = cwd.join("a\0b");
        refused(PtyCommand { cwd: &dir, ..clean }, "directory");
    }

    #[test]
    fn a_command_with_a_nul_in_an_argument_is_not_started() {
        // Cut at the NUL, this would have run `exit 0` on Windows and said
        // nothing of what was lost.
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec!["/c".to_string(), "exit 0\0& exit 1".to_string()],
            )
        } else {
            ("sh", vec!["-c".to_string(), "exit 0\0; exit 1".to_string()])
        };
        let env = BTreeMap::new();
        let cwd = std::env::temp_dir();

        let error = spawn(
            &PtyCommand {
                program,
                args: &args,
                env: &env,
                cwd: &cwd,
            },
            24,
            80,
        )
        .expect_err("a command cut short by a NUL is not started");
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{error}");
    }

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
