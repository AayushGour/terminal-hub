use hub_relay::paths::HubPaths;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::from_default_env()
    ).init();
    let paths = HubPaths::from_env();
    hub_daemon::server::run(paths).await
}
