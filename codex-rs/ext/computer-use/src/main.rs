use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use codex_computer_use_extension::ComputerUseServer;
use codex_computer_use_extension::DesktopSessionConfig;
use codex_computer_use_extension::LocalComputerUseRuntime;
use codex_computer_use_extension::LocalComputerUseRuntimeConfig;
use rmcp::ServiceExt;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long = "sky-bin")]
    sky_bin: PathBuf,

    #[arg(long = "xvfb")]
    xvfb: Option<PathBuf>,

    #[arg(long = "openbox")]
    openbox: Option<PathBuf>,

    #[arg(long = "temp-root")]
    temp_root: Option<PathBuf>,

    #[arg(long = "display-ready-timeout-ms")]
    display_ready_timeout_ms: Option<u64>,

    #[arg(long = "shutdown-grace-period-ms")]
    shutdown_grace_period_ms: Option<u64>,
}

fn stdio() -> (tokio::io::Stdin, tokio::io::Stdout) {
    (tokio::io::stdin(), tokio::io::stdout())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut session = DesktopSessionConfig::default();
    if let Some(xvfb) = args.xvfb {
        session.command_paths.xvfb = xvfb;
    }
    if let Some(openbox) = args.openbox {
        session.command_paths.openbox = openbox;
    }
    if let Some(temp_root) = args.temp_root {
        session.temp_root = temp_root;
    }
    if let Some(display_ready_timeout_ms) = args.display_ready_timeout_ms {
        session.display_ready_timeout = Duration::from_millis(display_ready_timeout_ms);
    }
    if let Some(shutdown_grace_period_ms) = args.shutdown_grace_period_ms {
        session.shutdown_grace_period = Duration::from_millis(shutdown_grace_period_ms);
    }
    let runtime = LocalComputerUseRuntime::new(LocalComputerUseRuntimeConfig {
        session,
        sky_binary_path: args.sky_bin,
        ..Default::default()
    });
    let service = ComputerUseServer::new(runtime);
    let running = service.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
