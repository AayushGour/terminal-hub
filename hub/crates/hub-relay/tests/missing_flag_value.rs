//! M3: a flag passed with no following value (e.g. `hub-relay --shell` with
//! nothing after it) must exit cleanly with a usage error, never panic.
//! Before the fix, `main.rs`'s arg parser called `it.next().unwrap()` on the
//! value fetch for --shell/--cwd/--term/--title, which would panic (and
//! print a Rust backtrace) instead of a clean error.

use std::process::{Command, Stdio};

fn relay_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-relay");
    p
}

#[test]
fn missing_value_for_shell_flag_exits_cleanly_no_panic() {
    let ok = Command::new("cargo").args(["build", "-p", "hub-relay"]).status().unwrap().success();
    assert!(ok, "build hub-relay");

    // `--shell` with nothing after it: old parser called it.next().unwrap().
    let out = Command::new(relay_bin())
        .args(["--shell"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run hub-relay");

    assert!(!out.status.success(), "missing flag value must be a non-zero exit, not silently accepted");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked at"), "must not panic; stderr: {stderr}");
}
