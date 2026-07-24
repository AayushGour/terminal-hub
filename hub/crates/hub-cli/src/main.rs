use clap::Parser;
use hub_cli::cli::{Cli, Command};
use hub_cli::{attach, install, kill, paths, status, uninstall, update};

fn main() {
    let cli = Cli::parse();
    match cli.command {
        // Never returns; must not build an async runtime before the exec.
        Command::Attach { new: _ } => attach::run_attach(),
        other => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let code = rt.block_on(async move {
                let home = paths::home_dir();
                let res = match other {
                    Command::Install { yes, bin_src, app_bundle } => {
                        install::run(&home, yes, bin_src.as_deref(), app_bundle.as_deref())
                    }
                    Command::Uninstall { yes, dry_run } => uninstall::run(&home, yes, dry_run).await,
                    Command::Update { yes, bin_src, app_bundle } => {
                        update::run(&home, yes, bin_src.as_deref(), app_bundle.as_deref()).await
                    }
                    Command::Status => status::run(&home).await,
                    Command::Kill { id } => kill::run(&home, id).await,
                    Command::Attach { .. } => unreachable!(),
                };
                match res {
                    Ok(()) => 0,
                    Err(e) => { eprintln!("hub: {e:#}"); 1 }
                }
            });
            std::process::exit(code);
        }
    }
}
