use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeKind {
    /// Append a `. ~/.bashrc` block to an existing login file.
    AppendSourceBashrc,
    /// Create ~/.bash_profile from scratch (source ~/.profile then ~/.bashrc).
    CreateProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcPlan {
    pub primary: PathBuf,
    pub bridge: Option<(PathBuf, BridgeKind)>,
}

pub fn plan_rc(
    shell: Shell,
    home: &Path,
    exists: &dyn Fn(&Path) -> bool,
    sources_bashrc: &dyn Fn(&Path) -> bool,
) -> RcPlan {
    match shell {
        Shell::Zsh => RcPlan {
            primary: home.join(".zshrc"),
            bridge: None,
        },
        Shell::Bash => {
            let primary = home.join(".bashrc");
            let profile = home.join(".bash_profile");
            let login = home.join(".bash_login");
            let bridge = if exists(&profile) {
                if sources_bashrc(&profile) {
                    None
                } else {
                    Some((profile, BridgeKind::AppendSourceBashrc))
                }
            } else if exists(&login) {
                if sources_bashrc(&login) {
                    None
                } else {
                    Some((login, BridgeKind::AppendSourceBashrc))
                }
            } else {
                Some((profile, BridgeKind::CreateProfile))
            };
            RcPlan { primary, bridge }
        }
    }
}
