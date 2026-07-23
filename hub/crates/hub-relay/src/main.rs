use hub_relay::detach;
use hub_relay::relay::RelayConfig;
use hub_proto::Origin;

struct Args {
    detach: bool,
    // Test-only affordance: proves reparenting from the surviving grandchild
    // in the `detach_reparent` integration test. Never compiled into release
    // builds.
    #[cfg(debug_assertions)]
    selftest_ppid: Option<String>,
    cfg: RelayConfig,
    daemon_sock: Option<String>,
}

/// Fetch the value following a flag, or fail cleanly (usage error to stderr +
/// non-zero exit) instead of panicking. A flag passed with nothing after it
/// (e.g. `hub-relay --shell` with no argument) must never crash the process.
fn require_value(it: &mut impl Iterator<Item = String>, flag: &str) -> String {
    match it.next() {
        Some(v) => v,
        None => {
            eprintln!("hub-relay: missing value for {flag}");
            std::process::exit(2);
        }
    }
}

fn parse() -> Args {
    let mut a = Args {
        detach: false, daemon_sock: None,
        #[cfg(debug_assertions)]
        selftest_ppid: None,
        cfg: RelayConfig {
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            cwd: std::env::var("PWD").unwrap_or_else(|_| "/".into()),
            env: vec![], cols: 80, rows: 24,
            term: "xterm-256color".into(),
            origin: Origin::External, title: "session".into(),
        },
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--detach" => a.detach = true,
            #[cfg(debug_assertions)]
            "--selftest-ppid" => a.selftest_ppid = it.next(),
            "--origin" => a.cfg.origin = match it.next().as_deref() {
                Some("hub") => Origin::Hub, _ => Origin::External,
            },
            "--shell" => a.cfg.shell = require_value(&mut it, "--shell"),
            "--cwd" => a.cfg.cwd = require_value(&mut it, "--cwd"),
            "--term" => a.cfg.term = require_value(&mut it, "--term"),
            "--title" => a.cfg.title = require_value(&mut it, "--title"),
            "--daemon-sock" => a.daemon_sock = it.next(),
            // `hub attach --new` (hub-cli) sends the real terminal size as
            // discrete --cols/--rows flags (see hub-cli/src/attach.rs). Keep
            // --size <WxH> too: Plan-2 tests and other call sites still use it.
            // If both are given, last-one-wins.
            "--cols" => {
                let v = require_value(&mut it, "--cols");
                a.cfg.cols = v.parse().unwrap_or(a.cfg.cols);
            }
            "--rows" => {
                let v = require_value(&mut it, "--rows");
                a.cfg.rows = v.parse().unwrap_or(a.cfg.rows);
            }
            "--size" => {
                if let Some(s) = it.next() {
                    if let Some((c, r)) = s.split_once('x') {
                        a.cfg.cols = c.parse().unwrap_or(80);
                        a.cfg.rows = r.parse().unwrap_or(24);
                    }
                }
            }
            _ => {}
        }
    }
    a
}

fn main() {
    let args = parse();

    // Detach BEFORE any tokio thread exists. Only the grandchild returns.
    if args.detach {
        detach::daemonize();
    }

    // Selftest hook: prove reparenting from the surviving grandchild.
    // Debug/test builds only — never shipped in release binaries.
    #[cfg(debug_assertions)]
    if let Some(path) = args.selftest_ppid {
        let ppid = nix::unistd::getppid().as_raw();
        std::fs::write(&path, format!("{ppid}")).ok();
        return;
    }

    // Real run: start tokio now (grandchild is single-threaded until here).
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        // Standalone binary: our real stdin IS the outer terminal, so the
        // External primary bridge is active (own_stdio = true).
        if let Err(e) = hub_relay::relay::run_relay(args.cfg, args.daemon_sock, true).await {
            tracing::error!("relay exited: {e:#}");
            std::process::exit(1);
        }
    });
    // The session has ended (shell exited / torn down). Exit immediately via
    // `_exit` rather than returning (which drops the multi-thread runtime) or
    // `std::process::exit` (which runs atexit/TLV destructors): `tokio::io::
    // {stdin,stdout}` keep blocking-pool threads parked in a `read`/`write`
    // syscall on the outer terminal's pipe/tty. Both graceful paths deadlock
    // waiting on those parked threads. `_exit` skips all handlers and cannot
    // hang. The relay's work is done; the kernel reclaims fds/threads.
    unsafe { libc::_exit(0) };
}
