use std::time::Duration;

fn relay_bin() -> std::path::PathBuf {
    // tests run with CWD = crate dir; bin is in the workspace target dir.
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-relay");
    p
}

#[test]
fn double_fork_reparents_to_init() {
    // Build the bin first so the path exists.
    let ok = std::process::Command::new("cargo")
        .args(["build", "-p", "hub-relay"]).status().unwrap().success();
    assert!(ok, "build hub-relay");

    let out = std::env::temp_dir().join(format!("ppid-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&out);

    // Launch detached selftest: it double-forks then writes getppid() to `out`.
    let status = std::process::Command::new(relay_bin())
        .args(["--detach", "--selftest-ppid", out.to_str().unwrap()])
        .status().unwrap();
    // The DIRECT child (original --detach process) exits 0 after forking.
    assert!(status.success());

    // The reparented grandchild writes the file shortly after.
    let mut ppid = String::new();
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(&out) { ppid = s; break; }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(ppid.trim(), "1", "grandchild must be reparented to init (pid 1)");
}
