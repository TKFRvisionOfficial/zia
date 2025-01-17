#![feature(entry_insert)]

use std::net::SocketAddr;
use clap::Parser;

use tokio::net::UdpSocket;
use tokio::select;
use tokio::signal::ctrl_c;
use tracing::info;
use url::Url;

use crate::cfg::ClientCfg;

mod cfg;
mod handler;
mod upstream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let config = ClientCfg::parse();

  tracing_subscriber::fmt::init();

  select! {
    result = tokio::spawn(listen(config.listen_addr, config.upstream, config.proxy)) => {
      result??;
      info!("Socket closed, quitting...");
    },
    result = shutdown_signal() => {
      result?;
      info!("Termination signal received, quitting...")
    }
  }

  Ok(())
}

async fn shutdown_signal() -> anyhow::Result<()> {
  let ctrl_c = async { ctrl_c().await.expect("failed to install Ctrl+C handler") };

  #[cfg(unix)]
  {
    let terminate = async {
      tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install signal handler")
        .recv()
        .await;
    };

    select! {
      _ = ctrl_c => {},
      _ = terminate => {},
    }

    Ok(())
  }

  #[cfg(not(unix))]
  {
    ctrl_c.await;
    Ok(())
  }
}

async fn listen(addr: SocketAddr, upstream: Url, proxy: Option<Url>) -> anyhow::Result<()> {
  let inbound = UdpSocket::bind(addr).await?;
  info!("Listening on {}/udp", inbound.local_addr()?);

  if let Some(proxy) = &proxy {
    info!("Using upstream at {} via proxy {}...", upstream, proxy);
  } else {
    info!("Using upstream at {}...", upstream);
  }

  upstream::transmit(inbound, &upstream, &proxy).await?;

  info!("Transmission via {} closed", upstream);

  Ok(())
}
