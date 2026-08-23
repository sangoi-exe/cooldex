use std::path::PathBuf;

use clap::Parser;
use codex_computer_use_extension::ComputerUseServer;
use codex_computer_use_extension::LocalComputerUseRuntime;
use codex_computer_use_extension::LocalComputerUseRuntimeConfig;
use rmcp::ServiceExt;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long = "sky-bin")]
    sky_bin: PathBuf,
}

fn stdio() -> (tokio::io::Stdin, tokio::io::Stdout) {
    (tokio::io::stdin(), tokio::io::stdout())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let runtime = LocalComputerUseRuntime::new(LocalComputerUseRuntimeConfig {
        sky_binary_path: args.sky_bin,
        ..Default::default()
    });
    let service = ComputerUseServer::new(runtime);
    let running = service.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
