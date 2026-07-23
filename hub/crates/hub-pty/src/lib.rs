//! hub-pty: portable-pty wrapper with a blocking-read -> mpsc bridge.

use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{
    native_pty_system, Child, CommandBuilder, MasterPty, PtySize as PortablePtySize,
};

#[derive(Clone, Copy, Debug)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

/// Owns the pty master + child handle. The blocking reader/waiter run on
/// internal threads; their output is delivered on `PtyOutput`.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    child_pid: Option<u32>,
}

pub struct PtyOutput {
    /// Blocking pty reads are bridged onto this channel by an internal thread.
    pub rx: Receiver<Vec<u8>>,
    /// Fires once with the exit code when the child exits (EOF on pty).
    pub exit_rx: Receiver<Option<i32>>,
}

impl Pty {
    /// Spawn `shell` in a fresh pty. `env` overrides/extends inherited env.
    pub fn spawn(
        shell: &str,
        cwd: &str,
        env: &[(String, String)],
        size: PtySize,
    ) -> anyhow::Result<(Pty, PtyOutput)> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PortablePtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = pair.slave.spawn_command(cmd)?;
        let child_pid = child.process_id();

        // Clone a blocking reader and take the writer BEFORE moving master into `Pty`.
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let child = Arc::new(Mutex::new(child));

        let (tx, rx) = channel::<Vec<u8>>();
        let (exit_tx, exit_rx) = channel::<Option<i32>>();

        // (1) Reader thread: bridge blocking pty reads onto the mpsc channel.
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,               // EOF: child closed the pty
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;                // receiver dropped
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,              // fd closed / error
                }
            }
        });

        // (2) Waiter thread: poll try_wait so kill() can always take the lock.
        let child_for_wait = Arc::clone(&child);
        thread::spawn(move || loop {
            let status = {
                let mut guard = child_for_wait.lock().unwrap();
                guard.try_wait()
            };
            match status {
                Ok(Some(exit)) => {
                    let code = exit.exit_code() as i32;
                    let _ = exit_tx.send(Some(code));
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => {
                    let _ = exit_tx.send(None);
                    break;
                }
            }
        });

        let pty = Pty {
            master: pair.master,
            writer,
            child,
            child_pid,
        };
        Ok((pty, PtyOutput { rx, exit_rx }))
    }

    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, size: PtySize) -> anyhow::Result<()> {
        self.master.resize(PortablePtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// Forcibly terminates the child via portable-pty `Child::kill()` (SIGKILL on unix).
    /// Graceful SIGHUP teardown is handled one layer up in hub-relay (see contract Addendum A).
    pub fn kill(&mut self) -> anyhow::Result<()> {
        let mut guard = self.child.lock().unwrap();
        guard.kill()?;
        Ok(())
    }
}

impl Drop for Pty {
    /// Safety net against leaking the shell + its reader/waiter threads on an
    /// early `?`-return elsewhere (e.g. hub-relay's `run_relay` can bail out
    /// AFTER the pty/shell is spawned but BEFORE the daemon dial, socket
    /// bind, or record write completes). Without this, dropping `Pty` on such
    /// a path left the child running with nothing left to reap it.
    ///
    /// Best-effort and panic-free, as required in a `Drop` impl: `.lock()`
    /// uses `if let Ok(..)` instead of `.unwrap()` so a poisoned mutex (e.g.
    /// from a panic elsewhere while holding the lock) can never turn this
    /// into a double panic during unwind, and the `kill()` result is ignored
    /// (it errors harmlessly if the child is already gone).
    ///
    /// This is a NET, not a replacement for normal teardown: the relay's
    /// regular exit path already SIGHUPs the shell one layer up
    /// (`sighup_shell` in hub-relay), and the waiter thread's `try_wait`
    /// typically reaps it before this `Pty` is ever dropped -- so on the
    /// normal path `kill()` here is just a harmless no-op (it errors when the
    /// pid no longer exists). It only does real work on the leak paths above.
    ///
    /// After this returns, the struct's fields drop in declaration order:
    /// `master` closes the pty master fd, which is what makes the reader
    /// thread's blocking `read()` observe EOF/error and exit on its own; the
    /// waiter thread exits once its `try_wait` observes the child gone (which
    /// our `kill()` call above hastens on the leak paths). Both are plain
    /// `std::thread`s with no join handle kept, so we don't block here to
    /// join them -- Drop must not block -- but neither can outlive the
    /// process meaningfully: they terminate promptly once the fd closes /
    /// the child dies.
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock() {
            let _ = guard.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Drain `rx` for up to `timeout`, returning all bytes seen as a String.
    fn collect_until(rx: &std::sync::mpsc::Receiver<Vec<u8>>, needle: &str, timeout: Duration) -> String {
        let start = Instant::now();
        let mut acc = String::new();
        while start.elapsed() < timeout {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                acc.push_str(&String::from_utf8_lossy(&chunk));
                if acc.contains(needle) {
                    break;
                }
            }
        }
        acc
    }

    #[test]
    fn spawn_write_read_echoes_output() {
        let (mut pty, out) = Pty::spawn(
            "/bin/sh",
            ".",
            &[("PS1".to_string(), "".to_string())],
            PtySize { cols: 80, rows: 24 },
        )
        .expect("spawn sh");
        pty.write(b"echo hub-marker-123\n").unwrap();
        let seen = collect_until(&out.rx, "hub-marker-123", Duration::from_secs(5));
        assert!(seen.contains("hub-marker-123"), "did not see echoed marker; saw: {seen:?}");
    }

    #[test]
    fn child_pid_is_present_after_spawn() {
        let (pty, _out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        assert!(pty.child_pid().is_some(), "expected a child pid");
    }

    #[test]
    fn resize_reports_new_size_via_stty() {
        let (mut pty, out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        pty.resize(PtySize { cols: 100, rows: 30 }).unwrap();
        // stty size prints "rows cols".
        pty.write(b"stty size\n").unwrap();
        let seen = collect_until(&out.rx, "30 100", Duration::from_secs(5));
        assert!(seen.contains("30 100"), "expected '30 100' from stty size; saw: {seen:?}");
    }

    #[test]
    fn child_exit_fires_exit_channel() {
        let (mut pty, out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        pty.write(b"exit 0\n").unwrap();
        let code = out
            .exit_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("exit channel should fire when the shell exits");
        assert_eq!(code, Some(0), "clean exit should report code 0");
    }

    #[test]
    fn kill_ends_the_shell() {
        let (mut pty, out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        let pid = pty.child_pid().expect("expected a child pid before kill");
        pty.kill().unwrap();
        // After kill, the waiter thread must eventually report an exit.
        let got = out.exit_rx.recv_timeout(Duration::from_secs(5));
        assert!(got.is_ok(), "exit channel should fire after kill");
        // The exit channel firing only proves the waiter thread's try_wait()
        // observed SOME status change; confirm the process is genuinely gone
        // by signalling it directly. kill(pid, 0) sends no signal but fails
        // with ESRCH once the pid no longer exists (and is reaped).
        let mut dead = false;
        for _ in 0..50 {
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                dead = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(dead, "child pid {pid} should no longer exist after kill()");
    }

    #[test]
    fn drop_kills_child() {
        // I2 regression: dropping a `Pty` (e.g. on an early `?`-return before
        // the daemon dial/socket bind/record write in `run_relay`) must not
        // leak the shell. Before the `Drop` impl, this pid would stay alive
        // (and its reader/waiter threads with it) forever.
        let (pty, _out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        let pid = pty.child_pid().expect("expected a child pid before drop");
        assert!(unsafe { libc::kill(pid as i32, 0) } == 0, "child should be alive before drop");

        drop(pty);

        // Same zombie-aware poll as `kill_ends_the_shell`: the Pty (via its
        // still-running waiter thread) owns the reap, so `try_wait` inside
        // that thread turns the killed child into a fully-gone pid rather
        // than a lingering zombie that `kill(pid, 0)` would still see as
        // "alive".
        let mut dead = false;
        for _ in 0..50 {
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                dead = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(dead, "child pid {pid} should no longer exist after dropping the Pty");
    }

    #[test]
    fn spawn_passes_env_to_child() {
        let (mut pty, out) = Pty::spawn(
            "/bin/sh",
            ".",
            &[("HUBTEST_VAR".to_string(), "hubval42".to_string())],
            PtySize { cols: 80, rows: 24 },
        )
        .expect("spawn sh");
        pty.write(b"printf '%s' \"$HUBTEST_VAR\"\n").unwrap();
        let seen = collect_until(&out.rx, "hubval42", Duration::from_secs(5));
        assert!(
            seen.contains("hubval42"),
            "expected env var HUBTEST_VAR to be visible to the child; saw: {seen:?}"
        );
    }
}
