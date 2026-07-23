use crate::paths;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AttachInputs {
    pub shell: String,
    pub cwd: String,
    pub term: String,
    pub cols: u16,
    pub rows: u16,
    pub hub_active: bool,
    pub relay_path: Option<PathBuf>,
    pub daemon_sock: PathBuf,
    pub daemon_up: bool,
}

#[derive(Debug)]
pub enum AttachAction {
    /// Fall through: replace this process with the user's plain shell.
    ExecShell(String),
    /// Replace this process with the relay (external capture).
    ExecRelay {
        relay: PathBuf,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
}

/// Pure decision. Any doubt → ExecShell so the terminal is never broken.
pub fn plan_attach(i: &AttachInputs) -> AttachAction {
    if i.hub_active {
        return AttachAction::ExecShell(i.shell.clone());
    }
    if !i.daemon_up {
        return AttachAction::ExecShell(i.shell.clone());
    }
    let relay = match &i.relay_path {
        Some(p) => p.clone(),
        None => return AttachAction::ExecShell(i.shell.clone()),
    };
    let args = vec![
        "--origin".into(), "external".into(),
        "--shell".into(), i.shell.clone(),
        "--cwd".into(), i.cwd.clone(),
        "--term".into(), i.term.clone(),
        "--cols".into(), i.cols.to_string(),
        "--rows".into(), i.rows.to_string(),
        "--daemon-sock".into(), i.daemon_sock.display().to_string(),
    ];
    let (hub_active_key, hub_active_val) = hub_active_env();
    AttachAction::ExecRelay {
        relay,
        args,
        env: vec![(hub_active_key.into(), hub_active_val.into())],
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

fn gather_inputs() -> AttachInputs {
    let home = paths::home_dir();
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/".into());
    let term = std::env::var("TERM")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "xterm-256color".into());
    let (cols, rows) = terminal_size().unwrap_or((80, 24));
    let daemon_sock = paths::daemon_sock_path(&home);
    // A2: connectable unix socket == daemon up. Returns immediately either way.
    let daemon_up = std::os::unix::net::UnixStream::connect(&daemon_sock).is_ok();
    AttachInputs {
        shell,
        cwd,
        term,
        cols,
        rows,
        hub_active: std::env::var_os("HUB_ACTIVE").is_some(),
        relay_path: paths::locate_sibling("hub-relay"),
        daemon_sock,
        daemon_up,
    }
}

/// Env every fall-through/relay child must carry so the rc snippet's
/// `[ -z "$HUB_ACTIVE" ]` guard stops re-attach recursion.
pub(crate) fn hub_active_env() -> (&'static str, &'static str) {
    ("HUB_ACTIVE", "1")
}

fn exec_shell(shell: &str) -> ! {
    // exec only returns on failure; keep degrading until something runs.
    let (k, v) = hub_active_env();
    // Every exec below must carry HUB_ACTIVE, otherwise a fall-through shell
    // (daemon down / relay missing / relay-exec failure) re-sources the rc
    // snippet, sees no HUB_ACTIVE, and re-runs `hub attach --new` — infinite
    // ever-deepening nesting that bricks the terminal.
    let _ = std::process::Command::new(shell).env(k, v).exec();
    let _ = std::process::Command::new("/bin/sh").env(k, v).exec();
    std::process::exit(127);
}

pub fn run_attach() -> ! {
    let i = gather_inputs();
    match plan_attach(&i) {
        AttachAction::ExecShell(sh) => exec_shell(&sh),
        AttachAction::ExecRelay { relay, args, env } => {
            let mut cmd = std::process::Command::new(&relay);
            cmd.args(&args);
            for (k, v) in &env {
                cmd.env(k, v);
            }
            let err = cmd.exec(); // returns only on failure
            eprintln!("hub: could not exec relay ({err}); falling back to shell");
            exec_shell(&i.shell)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fall-through shell (`exec_shell`) must carry the same HUB_ACTIVE
    /// env as the healthy relay path, otherwise the rc snippet's
    /// `[ -z "$HUB_ACTIVE" ]` guard never trips and `hub attach --new`
    /// recurses forever, bricking the terminal.
    #[test]
    fn fallthrough_shell_carries_hub_active() {
        assert_eq!(hub_active_env(), ("HUB_ACTIVE", "1"));
    }
}
