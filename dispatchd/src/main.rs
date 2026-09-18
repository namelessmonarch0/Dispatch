//! `dispatchd`: the process that owns the agents.
//!
//! Runs in the foreground and logs to a file. Nothing here daemonizes: a
//! supervisor — launchd, systemd, or the Dispatch client starting one on
//! demand — decides how the process is detached, and a daemon that forks
//! itself cannot be debugged by running it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use dispatch_config::HarnessRegistry;
use dispatch_daemon::{Daemon, Shutdown};
use dispatch_os::ipc::Listener;

/// The process that owns Dispatch's agents.
#[derive(Debug, Parser)]
#[command(name = "dispatchd", version, about)]
struct Args {
    /// Projects to serve. Defaults to the current directory.
    projects: Vec<PathBuf>,

    /// Write diagnostics to this file instead of the default location.
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// Name this daemon reports to clients, so two can be told apart.
    #[arg(long, default_value = "local")]
    device: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.log_file.clone())?;

    // Written on first run and never overwritten, so local edits survive. The
    // daemon is the process that starts agents, so it is the one that needs
    // the definitions.
    let harness_dir =
        dispatch_os::paths::harnesses_dir().context("failed to locate the harness directory")?;
    let written = dispatch_config::write_missing_built_ins(&harness_dir)
        .context("failed to write the built-in harnesses")?;
    if !written.is_empty() {
        tracing::info!(?written, "wrote built-in harnesses");
    }

    let harnesses = HarnessRegistry::load_from_dir(&harness_dir)
        .context("failed to load harness definitions")?;
    tracing::info!(count = harnesses.len(), "harnesses registered");

    let mut daemon = Daemon::new(harnesses, args.device.clone());

    let projects = if args.projects.is_empty() {
        vec![std::env::current_dir().context("failed to read the working directory")?]
    } else {
        args.projects
    };
    for project in projects {
        let root = project
            .canonicalize()
            .with_context(|| format!("no such directory: {}", project.display()))?;
        let id = daemon.open_project(root.clone());
        tracing::info!(project = %id, root = %root.display(), "serving a project");
    }

    // Taken before serving, which consumes the daemon.
    let shutdown = daemon.shutdown_handle();
    watch_for_shutdown(shutdown);

    // Binding fails rather than replacing a running daemon, because two
    // daemons on one endpoint would each own half the fleet.
    let listener = Listener::bind().context("failed to start listening")?;
    let endpoint = dispatch_os::ipc::endpoint().context("failed to locate the endpoint")?;

    // The only thing written to a terminal: enough for someone who started it
    // by hand to know it came up, and where to look for the rest.
    tracing::info!(endpoint = %endpoint.display(), device = %args.device, "listening");
    eprintln!("dispatchd listening on {}", endpoint.display());

    daemon.serve(listener).context("the daemon stopped")?;

    tracing::info!("stopped");
    Ok(())
}

/// Stops the daemon when the operating system asks the process to exit.
///
/// The loop is synchronous and the signal watcher is not, so the watcher gets
/// its own thread with its own runtime rather than making the whole daemon
/// async for one future.
fn watch_for_shutdown(shutdown: Shutdown) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                // Without this the daemon still serves; it just has to be
                // killed rather than asked to stop.
                tracing::warn!(%error, "failed to watch for shutdown signals");
                return;
            }
        };

        if let Err(error) = runtime.block_on(dispatch_os::signal::shutdown_requested()) {
            tracing::warn!(%error, "failed to watch for shutdown signals");
            return;
        }

        tracing::info!("shutdown requested");
        shutdown.request();
    });
}

/// Sends diagnostics to a file.
///
/// Separate from the client's log: the two are different processes, and
/// interleaving their lines makes both harder to read.
fn init_logging(override_path: Option<PathBuf>) -> Result<()> {
    use tracing_subscriber::EnvFilter;

    let path = match override_path {
        Some(path) => path,
        None => dispatch_os::paths::daemon_log_file().context("failed to locate the log file")?,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;

    tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    Ok(())
}
