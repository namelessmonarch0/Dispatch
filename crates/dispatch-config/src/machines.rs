//! The machines a user has registered.
//!
//! A machine is somewhere Dispatch reaches a daemon by running a command —
//! `ssh <target> dispatchd --stdio` unless told otherwise. The list is the
//! user's: nothing here dials, and nothing is on it that `dispatch machine
//! add` or the add overlay did not put there.

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// The file the list lives in, inside the configuration directory.
const FILE: &str = "machines.toml";

/// What ssh is told before the target.
///
/// `-T` because the bytes are frames, not a session. `BatchMode` because
/// without it ssh asks for a password or a host key on `/dev/tty` — the very
/// terminal the interface is drawn on — and the dial hangs behind the prompt.
/// With it ssh fails at once, saying why on stderr, which the client already
/// surfaces. `ConnectTimeout` so an asleep host fails a dial in seconds rather
/// than the kernel's TCP timeout.
const SSH_OPTIONS: &[&str] = &["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"];

/// What ssh runs on the far side.
const REMOTE: &[&str] = &["dispatchd", "--stdio"];

/// A machine Dispatch can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// What the sidebar and the command line call it.
    pub name: String,
    /// Where ssh connects: a host, `user@host`, or an alias from the user's
    /// ssh configuration.
    pub target: String,
    /// A command that replaces the default ssh one entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
}

/// A program and its arguments, kept apart.
///
/// Apart so a local program whose path holds a space can be named, which a
/// whitespace-split string cannot do. Strings rather than `OsString`s: serde
/// writes an `OsString` into TOML as a platform-tagged byte array nobody could
/// edit by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// The program to run.
    pub program: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// The file's shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    /// Machines in the order they were added. `[[machine]]` in the file,
    /// because each entry is one machine.
    #[serde(default, rename = "machine")]
    machines: Vec<Machine>,
}

impl Machine {
    /// A machine reached by the default ssh command.
    #[must_use]
    pub fn new(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            target: target.into(),
            command: None,
        }
    }

    /// The program and arguments that reach this machine's daemon.
    #[must_use]
    pub fn dial(&self) -> (OsString, Vec<OsString>) {
        if let Some(command) = &self.command {
            return (
                OsString::from(&command.program),
                command.args.iter().map(OsString::from).collect(),
            );
        }

        let mut args: Vec<OsString> = SSH_OPTIONS.iter().map(OsString::from).collect();
        args.push(OsString::from(&self.target));
        args.extend(REMOTE.iter().map(OsString::from));
        (OsString::from("ssh"), args)
    }

    /// The command as one line, for a message that has to say what was run.
    #[must_use]
    pub fn describe(&self) -> String {
        let (program, args) = self.dial();
        let mut line = program.to_string_lossy().into_owned();
        for arg in args {
            line.push(' ');
            line.push_str(&arg.to_string_lossy());
        }
        line
    }
}

/// Whether `name` can name a machine.
///
/// It is a TOML table key in `projects.toml` and a word typed on the command
/// line, so nothing that would need quoting in either.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A name for the machine at `target`, when one can be made from it.
///
/// The host, cut at its first dot: `tower` from `me@tower.lan`. An IPv4
/// address keeps all four parts with dashes, because its first part alone
/// would name every machine on the subnet the same.
#[must_use]
pub fn default_name(target: &str) -> Option<String> {
    let host = target.strip_prefix("ssh://").unwrap_or(target);
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);

    let name = if host.parse::<std::net::Ipv4Addr>().is_ok() {
        host.replace('.', "-")
    } else {
        host.split('.').next().unwrap_or_default().to_string()
    };

    valid_name(&name).then_some(name)
}

/// The registered machines, oldest first.
///
/// No file means nothing is registered, which is what a first run looks like
/// rather than a failure.
pub fn load(dir: &Path) -> Result<Vec<Machine>, ConfigError> {
    let path = dir.join(FILE);

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ConfigError::Io { path, source }),
    };

    let saved: Saved = toml::from_str(&text).map_err(|source| ConfigError::Toml {
        path: path.clone(),
        source,
    })?;

    Ok(saved.machines)
}

/// Writes the list, replacing whatever was there.
pub fn save(dir: &Path, machines: &[Machine]) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let path = dir.join(FILE);
    let text = toml::to_string_pretty(&Saved {
        machines: machines.to_vec(),
    })
    .expect("a list of machines serialises");

    std::fs::write(&path, text).map_err(|source| ConfigError::Io { path, source })
}

/// Whether `name` could be registered now.
///
/// Its own step so a caller can ask before spending thirty seconds proving the
/// machine answers, only to be told the name was taken.
pub fn check(dir: &Path, name: &str, this_host: &str) -> Result<(), ConfigError> {
    let refuse = |reason: String| ConfigError::Machine {
        path: dir.join(FILE),
        reason,
    };

    if !valid_name(name) {
        return Err(refuse(format!(
            "{name:?} is not a machine name; use letters, digits, - and _"
        )));
    }

    // Compared with the host's first label too: `laptop` is this machine
    // whether the operating system says `laptop` or `Laptop.local`.
    let short = this_host.split('.').next().unwrap_or(this_host);
    if name.eq_ignore_ascii_case(this_host) || name.eq_ignore_ascii_case(short) {
        return Err(refuse(format!("{name} is this machine's own name")));
    }

    if load(dir)?.iter().any(|machine| machine.name == name) {
        return Err(refuse(format!("{name} is already registered")));
    }

    Ok(())
}

/// Registers `machine`, refusing a name [`check`] would refuse.
pub fn add(dir: &Path, machine: Machine, this_host: &str) -> Result<(), ConfigError> {
    check(dir, &machine.name, this_host)?;

    let mut machines = load(dir)?;
    machines.push(machine);
    save(dir, &machines)
}

/// Takes the machine called `name` off the list.
///
/// Answers whether it was there to take off.
pub fn remove(dir: &Path, name: &str) -> Result<bool, ConfigError> {
    let mut machines = load(dir)?;
    let before = machines.len();

    machines.retain(|machine| machine.name != name);
    if machines.len() == before {
        return Ok(false);
    }

    save(dir, &machines)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
