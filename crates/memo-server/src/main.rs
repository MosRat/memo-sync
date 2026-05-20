use clap::Parser;
use memo_server::{open_file_pool, router, state};
use std::{net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(all(target_os = "linux", not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "memo-server", about = "Memo Sync backend")]
struct Args {
    #[arg(long, env = "MEMO_BIND", default_value = "127.0.0.1:7373")]
    bind: SocketAddr,
    #[arg(long, env = "MEMO_DATABASE", default_value = "memo-server.sqlite")]
    database: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    let pool = open_file_pool(args.database).await?;
    let listener = TcpListener::bind(args.bind).await?;
    tracing::info!(addr = %args.bind, "memo sync server listening");
    axum::serve(listener, router(state(pool))).await?;
    Ok(())
}
