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
    ///
    /// The hostname by default: a fleet whose rows all say "local" is a fleet
    /// you cannot read. Asked of the operating system rather than the
    /// environment, because `$HOSTNAME` is a bash-only convention that most
    /// shells and most CI runners never set.
    #[arg(long, default_value_t = dispatch_os::host::hostname())]
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

    // Absent means default. Unknown keys are logged rather than rejected: a
    // newer Dispatch's key must not stop an older daemon starting, and a typo
    // must not be silent either.
    let config_path =
        dispatch_os::paths::config_file().context("failed to locate the configuration file")?;
    let loaded = dispatch_config::Config::load_reporting(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    if !loaded.unknown.is_empty() {
        tracing::warn!(
            keys = ?loaded.unknown,
            path = %config_path.display(),
            "ignoring unknown configuration keys"
        );
    }
    tracing::info!(
        max_depth = loaded.config.delegation.max_depth,
        max_live_per_parent = loaded.config.delegation.max_live_per_parent,
        request_timeout_secs = loaded.config.delegation.request_timeout_secs,
        "delegation limits"
    );

    let mut daemon = Daemon::with_limits(harnesses, args.device.clone(), loaded.config.delegation);

    let projects = if args.projects.is_empty() {
        vec![std::env::current_dir().context("failed to read the working directory")?]
    } else {
        args.projects
    };
    for project in projects {
        let root = dispatch_os::paths::resolve(&project)
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

    // Written while listening and removed on the way out. The endpoint says
    // whether a daemon is answering; this says which process to stop, which is
    // what an operator — or a client that started one — needs.
    let pid_file = PidFile::write().context("failed to record the daemon's process id")?;

    // The only thing written to a terminal: enough for someone who started it
    // by hand to know it came up, and where to look for the rest.
    tracing::info!(endpoint = %endpoint.display(), device = %args.device, "listening");
    eprintln!("dispatchd listening on {}", endpoint.display());

    daemon.serve(listener).context("the daemon stopped")?;

    drop(pid_file);
    tracing::info!("stopped");
    Ok(())
}

/// The daemon's process id on disk, removed when the daemon stops.
struct PidFile(PathBuf);

impl PidFile {
    fn write() -> Result<Self> {
        let path = dispatch_os::paths::daemon_pid_file()?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        std::fs::write(&path, std::process::id().to_string())
            .with_context(|| format!("failed to write {}", path.display()))?;

        Ok(Self(path))
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // A file left behind would name a process that is gone. Nothing depends
        // on it being removed, so a failure is logged rather than raised.
        if let Err(error) = std::fs::remove_file(&self.0) {
            tracing::warn!(%error, path = %self.0.display(), "failed to remove the pid file");
        }
    }
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
