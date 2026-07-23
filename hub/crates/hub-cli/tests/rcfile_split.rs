use hub_cli::rcfile::{plan_rc, BridgeKind, Shell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn fs(existing: &[&str], sources: &[&str]) -> (impl Fn(&Path) -> bool, impl Fn(&Path) -> bool) {
    let ex: HashSet<PathBuf> = existing.iter().map(PathBuf::from).collect();
    let sr: HashSet<PathBuf> = sources.iter().map(PathBuf::from).collect();
    (move |p: &Path| ex.contains(p), move |p: &Path| sr.contains(p))
}

#[test]
fn zsh_targets_zshrc_no_bridge() {
    let home = Path::new("/h");
    let (e, s) = fs(&[], &[]);
    let plan = plan_rc(Shell::Zsh, home, &e, &s);
    assert_eq!(plan.primary, PathBuf::from("/h/.zshrc"));
    assert!(plan.bridge.is_none());
}

#[test]
fn bash_primary_is_bashrc() {
    let home = Path::new("/h");
    let (e, s) = fs(&[], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(plan.primary, PathBuf::from("/h/.bashrc"));
}

#[test]
fn bash_profile_exists_and_sources_bashrc_needs_no_bridge() {
    let home = Path::new("/h");
    let (e, s) = fs(&["/h/.bash_profile"], &["/h/.bash_profile"]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert!(plan.bridge.is_none());
}

#[test]
fn bash_profile_exists_without_sourcing_gets_append_bridge() {
    let home = Path::new("/h");
    let (e, s) = fs(&["/h/.bash_profile"], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(
        plan.bridge,
        Some((PathBuf::from("/h/.bash_profile"), BridgeKind::AppendSourceBashrc))
    );
}

#[test]
fn bash_no_login_file_creates_bash_profile() {
    let home = Path::new("/h");
    let (e, s) = fs(&[], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(
        plan.bridge,
        Some((PathBuf::from("/h/.bash_profile"), BridgeKind::CreateProfile))
    );
}

#[test]
fn bash_login_used_when_no_profile() {
    let home = Path::new("/h");
    let (e, s) = fs(&["/h/.bash_login"], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(
        plan.bridge,
        Some((PathBuf::from("/h/.bash_login"), BridgeKind::AppendSourceBashrc))
    );
}
