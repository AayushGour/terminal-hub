// Generates and installs an OS autostart entry that launches `hub-daemon` on
// login: a launchd user LaunchAgent plist on macOS, a systemd user unit on
// Linux. Content-generation (`launchd_plist`, `systemd_unit`) is pure and
// OS-independent so both are unit-tested regardless of the host OS; only the
// filesystem-touching / launchctl-or-systemctl-calling `install_autostart`
// and `remove_autostart` paths are `#[cfg]`-gated per OS. Integration tests
// that drive the whole `install::run`/`uninstall::run` set
// `HUB_SKIP_SERVICE_ACTIVATION` (see `skip_activation`), which keeps the
// plist/unit file write+remove (so paths stay verifiable) but suppresses the
// `launchctl`/`systemctl` shell-out — so the test suite never runs
// `launchctl`/`systemctl` against the real system.
#![allow(dead_code)]

use crate::manifest::AutostartEntry;
use std::path::Path;
use std::process::Command;

pub const LAUNCHD_LABEL: &str = "com.hub.daemon";
pub const SYSTEMD_NAME: &str = "hub-daemon.service";

/// Testability seam: when this env var is set, `install_autostart` /
/// `remove_autostart` still write/remove the plist-or-unit file (so the
/// generated content and recorded path stay verifiable) but SKIP the
/// `launchctl`/`systemctl` shell-out that would register/deregister the agent
/// against the real service manager. Integration tests that drive the whole
/// `install::run`/`uninstall::run` (e.g. `--bin-src` self-contained install)
/// set this so they never mutate the host's launchd/systemd. Never set in
/// production.
const SKIP_ACTIVATION_ENV: &str = "HUB_SKIP_SERVICE_ACTIVATION";

fn skip_activation() -> bool {
    std::env::var_os(SKIP_ACTIVATION_ENV).is_some()
}

pub fn kind_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "launchd"
    } else {
        "systemd --user"
    }
}

/// Escapes text for placement inside a plist XML text node (e.g. `<string>...</string>`).
/// `&` must be replaced first so the entities introduced by the other
/// replacements are not themselves re-escaped.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Quotes a value for use as (part of) a systemd unit `ExecStart=` line so
/// that whitespace in the value doesn't get word-split by systemd. Backslash
/// and embedded double-quotes are escaped per systemd's quoting rules.
fn systemd_quote(s: &str) -> String {
    let inner = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{inner}\"")
}

pub fn launchd_plist(program: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{prog}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
</dict>
</plist>
"#,
        label = xml_escape(LAUNCHD_LABEL),
        prog = xml_escape(&program.display().to_string())
    )
}

pub fn systemd_unit(program: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Terminal Hub daemon (router/registry)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={prog}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        prog = systemd_quote(&program.display().to_string())
    )
}

#[cfg(target_os = "macos")]
pub fn install_autostart(daemon: &Path, home: &Path) -> anyhow::Result<AutostartEntry> {
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let plist_path = dir.join(format!("{LAUNCHD_LABEL}.plist"));
    std::fs::write(&plist_path, launchd_plist(daemon))?;
    // Modern bootstrap; ignore "already bootstrapped" errors.
    if !skip_activation() {
        let uid = unsafe { libc::getuid() };
        let _ = Command::new("launchctl")
            .args([
                "bootstrap",
                &format!("gui/{uid}"),
                &plist_path.display().to_string(),
            ])
            .status();
    }
    Ok(AutostartEntry::Launchd {
        plist: plist_path.display().to_string(),
        label: LAUNCHD_LABEL.to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn install_autostart(daemon: &Path, home: &Path) -> anyhow::Result<AutostartEntry> {
    let dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    let unit_path = dir.join(SYSTEMD_NAME);
    std::fs::write(&unit_path, systemd_unit(daemon))?;
    if !skip_activation() {
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = Command::new("systemctl")
            .args(["--user", "enable", "--now", SYSTEMD_NAME])
            .status();
    }
    Ok(AutostartEntry::Systemd {
        unit: unit_path.display().to_string(),
        name: SYSTEMD_NAME.to_string(),
    })
}

pub fn remove_autostart(entry: &AutostartEntry) -> anyhow::Result<()> {
    let skip = skip_activation();
    match entry {
        AutostartEntry::Launchd { plist, label } => {
            if !skip {
                let uid = unsafe { libc::getuid() };
                let _ = Command::new("launchctl")
                    .args(["bootout", &format!("gui/{uid}/{label}")])
                    .status();
            }
            let _ = std::fs::remove_file(plist);
        }
        AutostartEntry::Systemd { unit, name } => {
            if !skip {
                let _ = Command::new("systemctl")
                    .args(["--user", "disable", "--now", name])
                    .status();
            }
            let _ = std::fs::remove_file(unit);
            if !skip {
                let _ = Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .status();
            }
        }
    }
    Ok(())
}
