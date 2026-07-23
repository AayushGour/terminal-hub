use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A throwaway HOME with a fake `hub` on PATH and a call-log file.
pub struct TempHome {
    pub dir: tempfile::TempDir,
    pub bin: PathBuf,
    pub call_log: PathBuf,
}

impl TempHome {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        let call_log = dir.path().join("hub_calls.log");

        // Fake `hub`: logs its args, exits with FAKE_HUB_EXIT (default 1 = daemon down).
        let hub = bin.join("hub");
        std::fs::write(
            &hub,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HUB_CALL_LOG\"\nexit \"${FAKE_HUB_EXIT:-1}\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        TempHome { dir, bin, call_log }
    }

    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    pub fn calls(&self) -> String {
        std::fs::read_to_string(&self.call_log).unwrap_or_default()
    }

    /// Overwrite the fake `hub` with a custom shell script body (must start with
    /// a shebang). Used by the no-infinite-nesting gate to install a `hub` that
    /// *execs a fall-through shell* (mimicking production `exec_shell`) so the
    /// gate can prove the rc snippet's re-exec guard actually bounds recursion.
    #[allow(dead_code)]
    pub fn set_fake_hub(&self, body: &str) {
        let hub = self.bin.join("hub");
        std::fs::write(&hub, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// Result of driving a shell.
pub struct ShellProbe {
    pub output: String,
}

impl ShellProbe {
    pub fn reached_prompt(&self) -> bool {
        self.output.contains("HUB_TEST_OK")
    }
}

/// Spawn `program args` in a real pty with the given env, write `drive` lines,
/// read until "HUB_TEST_OK" or timeout. A tty is present so `[ -t 1 ]` is true.
pub fn run_in_pty(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    drive: &[&str],
    timeout: Duration,
) -> ShellProbe {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .unwrap();

    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    for line in drive {
        writeln!(writer, "{line}").unwrap();
    }
    writer.flush().unwrap();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut acc = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&chunk[..n]);
                    let _ = tx.send(acc.clone());
                }
                Err(_) => break,
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut out = String::new();
    while Instant::now() < deadline {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
            out = String::from_utf8_lossy(&bytes).to_string();
            if out.contains("HUB_TEST_OK") {
                break;
            }
        }
    }
    let _ = child.kill();
    ShellProbe { output: out }
}
