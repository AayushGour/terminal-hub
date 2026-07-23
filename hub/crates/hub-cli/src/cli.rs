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
    },
    /// Full clean: restore rc files, stop daemon, remove autostart + ~/.hub.
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
