//! Full detach from birth (spec §6): setsid + double-fork so the surviving
//! grandchild is reparented to init/launchd (pid 1) and can never re-acquire a
//! controlling terminal. MUST be called in `main` BEFORE the tokio runtime
//! starts, so the fork happens in a single-threaded process (no locked-mutex
//! hazard). Only async-signal-safe calls are made between fork and the point
//! the grandchild returns.

use nix::sys::wait::waitpid;
use nix::unistd::{fork, getppid, setsid, ForkResult};

/// Returns ONLY in the reparented grandchild. All other processes `_exit`.
pub fn daemonize() {
    // Level 0: the original `hub-relay --detach` process (the daemon's direct
    // child). Fork the session leader, wait for it, then exit — so the daemon's
    // wait() reaps a clean child and never becomes the relay's ancestor.
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            let _ = waitpid(child, None);
            unsafe { libc::_exit(0) };
        }
        Ok(ForkResult::Child) => { /* fall through: become session leader */ }
        Err(_) => unsafe { libc::_exit(70) },
    }

    // Level 1: new session leader (detached from any controlling terminal).
    if setsid().is_err() {
        unsafe { libc::_exit(71) };
    }

    // Double-fork: the grandchild is NOT a session leader, so it can never
    // acquire a controlling terminal. When THIS level-1 child exits, the
    // grandchild is reparented to init (pid 1).
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            unsafe { libc::_exit(0) };
        }
        Ok(ForkResult::Child) => { /* grandchild: continue below */ }
        Err(_) => unsafe { libc::_exit(72) },
    }

    // Grandchild: wait until reparenting has actually taken effect. There is a
    // brief window where getppid() is still the (exiting) level-1 pid.
    for _ in 0..1000 {
        if getppid().as_raw() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // M4: redirect fds 0/1/2 to /dev/null now that we are the surviving,
    // reparented grandchild. Without this, a detached (Hub-origin, --detach)
    // relay silently inherits the SPAWNING process's real stdin/stdout/stderr
    // (fork just duplicates the fd table) and keeps that tty/pipe open for
    // its entire lifetime even after the spawner exits -- and could write
    // stray output into it. This is gated to ONLY the detach path: `daemonize`
    // is called from `main` solely when `--detach` is passed (see main.rs),
    // which is exclusively the Hub-origin flow. External-origin relays never
    // call `daemonize` at all, so their fds 0/1/2 are untouched -- correctly,
    // since for them fds 0/1/2 ARE the outer terminal being relayed.
    //
    // Done here (after the double-fork, before `main` starts tokio) using raw
    // libc calls only -- open/dup2/close are async-signal-safe and the
    // process is still single-threaded at this point, preserving the
    // fork-safety invariant documented at the top of this file.
    redirect_stdio_to_dev_null();

    // Return to caller (main), which now starts tokio and runs the relay.
}

/// Best-effort: point fds 0, 1, and 2 at `/dev/null`. No heap allocation (a
/// `'static` nul-terminated byte string, not `CString::new`) and no panics --
/// this runs in the fragile pre-tokio, post-double-fork window, so a failure
/// to open/dup2 is simply ignored rather than aborting the relay.
fn redirect_stdio_to_dev_null() {
    const DEV_NULL: &[u8] = b"/dev/null\0";
    let fd = unsafe { libc::open(DEV_NULL.as_ptr() as *const libc::c_char, libc::O_RDWR) };
    if fd < 0 {
        return;
    }
    unsafe {
        libc::dup2(fd, 0);
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
        if fd > 2 {
            libc::close(fd);
        }
    }
}
