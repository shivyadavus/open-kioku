#[tokio::main]
async fn main() -> anyhow::Result<()> {
    open_kioku_cli::run_cli().await
}
