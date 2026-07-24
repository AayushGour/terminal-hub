use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hub", about = "Terminal Hub control CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Capture the current terminal (called by the injected shell-rc snippet).
    Attach {
        /// Spawn/attach a NEW relay for this terminal.
        #[arg(long)]
        new: bool,
    },
    /// One-time consent-gated setup (rc injection + autostart + ~/.hub).
    Install {
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Self-contained mode: copy `hub`, `hub-daemon`, `hub-relay` from this
        /// directory into `~/.hub/bin` (0755) so the install survives the
        /// installing app bundle being deleted. Without it, `hub` is assumed to
        /// already be on PATH and no binaries are placed.
        #[arg(long, value_name = "DIR")]
        bin_src: Option<PathBuf>,
        /// Path to a built `.app` bundle (e.g.
        /// `target/release/bundle/macos/hub.app`) to copy into `/Applications`
        /// alongside the CLI install. macOS only. Without it, install is
        /// CLI-only (unchanged prior behavior) and no GUI app is placed.
        #[arg(long, value_name = "APP_BUNDLE")]
        app_bundle: Option<PathBuf>,
    },
    /// Update an existing install to a freshly built version in place,
    /// without disrupting any live terminal session (see `update::run`'s
    /// module doc for the safety mechanism). Requires a prior `hub install`.
    Update {
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Self-contained mode: copy `hub`, `hub-daemon`, `hub-relay` from
        /// this directory into `~/.hub/bin` (0755), replacing the currently
        /// installed copies. Without it, the binaries on disk are left
        /// unchanged and only the daemon is restarted (and/or the app bundle
        /// updated).
        #[arg(long, value_name = "DIR")]
        bin_src: Option<PathBuf>,
        /// Path to a freshly built `.app` bundle to copy into
        /// `/Applications`, replacing the one already there. macOS only.
        /// Without it, any existing installed app bundle is left untouched.
        #[arg(long, value_name = "APP_BUNDLE")]
        app_bundle: Option<PathBuf>,
    },
    /// Full clean: restore rc files, stop daemon, remove autostart + ~/.hub
    /// + the `.app` bundle (if `hub install --app-bundle` placed one).
    Uninstall {
        #[arg(long)]
        yes: bool,
        /// List everything that would be touched, change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// List sessions (healthy / ghost / orphan buckets).
    Status,
    /// Kill a session by id.
    Kill { id: u64 },
}
