//! `dispatch machine`: the machine registry's command-line verbs.
//!
//! None of these draw an interface or start a local daemon. `add` dials the
//! machine once, to prove it has a `dispatchd` that answers, and hangs up.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use dispatch_client::{Client, Liveness};
use dispatch_config::machines::{self, Machine};
use dispatch_proto::Role;

/// What to do to the registry.
#[derive(Debug, clap::Subcommand)]
pub enum Action {
    /// Register a machine, after proving its daemon answers.
    Add {
        /// Where ssh connects: a host, `user@host`, or an alias from your ssh
        /// configuration.
        target: String,

        /// What to call it. Defaults to the target's host.
        #[arg(long)]
        name: Option<String>,

        /// Save it without dialling it, for a machine that is asleep now.
        #[arg(long)]
        no_check: bool,

        /// The command that reaches its daemon, in place of
        /// `ssh <target> dispatchd --stdio`. Given after `--`.
        #[arg(last = true, value_name = "PROGRAM ARGS")]
        command: Vec<String>,
    },

    /// List the registered machines. Dials nothing.
    List,

    /// Forget a machine. Its daemon and agents are left running.
    Remove {
        /// The machine's name, as `list` prints it.
        name: String,
    },
}

/// Runs one verb against this user's configuration.
pub fn run(action: Action) -> Result<ExitCode> {
    let dir = dispatch_os::paths::config_dir().context("failed to locate the config dir")?;

    match action {
        Action::Add {
            target,
            name,
            no_check,
            command,
        } => add(&dir, target, name, no_check, command),
        Action::List => list(&dir),
        Action::Remove { name } => remove(&dir, &name),
    }
}

fn add(
    dir: &Path,
    target: String,
    name: Option<String>,
    no_check: bool,
    command: Vec<String>,
) -> Result<ExitCode> {
    let Some(name) = name.or_else(|| machines::default_name(&target)) else {
        eprintln!("cannot make a machine name from {target:?}; pass --name");
        return Ok(ExitCode::from(1));
    };

    let mut words = command.into_iter();
    let command = words.next().map(|program| machines::Command {
        program,
        args: words.collect(),
    });
    let machine = Machine {
        name,
        target,
        command,
    };
    let host = dispatch_os::host::hostname();

    // Before the dial: being told the name is taken should not cost the
    // thirty seconds a dial to a slow host can take.
    if let Err(error) = machines::check(dir, &machine.name, &host) {
        eprintln!("{error}");
        return Ok(ExitCode::from(1));
    }

    let said = if no_check {
        format!("added {} without checking it", machine.name)
    } else {
        let (program, args) = machine.dial();
        match Client::attach_over(
            Role::Interface,
            crate::CLIENT_NAME,
            Liveness::default(),
            program,
            args,
        ) {
            // Dropped at once: the check was whether it answers, and
            // dropping the client ends the command it started.
            Ok(client) => format!(
                "added {} (its daemon calls itself {})",
                machine.name,
                client.device()
            ),
            Err(error) => {
                eprintln!("could not reach {}: {error}", machine.name);
                eprintln!("ran: {}", machine.describe());
                return Ok(ExitCode::from(1));
            }
        }
    };

    if let Err(error) = machines::add(dir, machine, &host) {
        eprintln!("{error}");
        return Ok(ExitCode::from(1));
    }

    println!("{said}");
    Ok(ExitCode::SUCCESS)
}

fn list(dir: &Path) -> Result<ExitCode> {
    let registered = machines::load(dir)?;

    if registered.is_empty() {
        println!("no machines registered; add one with `dispatch machine add <ssh-target>`");
        return Ok(ExitCode::SUCCESS);
    }

    for machine in registered {
        let how = match &machine.command {
            Some(_) => machine.describe(),
            None => machine.target.clone(),
        };
        println!("{}\t{how}", machine.name);
    }

    Ok(ExitCode::SUCCESS)
}

fn remove(dir: &Path, name: &str) -> Result<ExitCode> {
    if !machines::remove(dir, name)? {
        eprintln!("no machine named {name}");
        return Ok(ExitCode::from(1));
    }

    dispatch_config::projects::forget_machine(dir, name)?;
    println!("removed {name}; its daemon and agents on that machine were left running");
    Ok(ExitCode::SUCCESS)
}
