use std::net::SocketAddr;

use clap::Parser;
use url::Url;

#[derive(Parser)]
#[clap(version)]
pub(crate) struct ClientCfg {
  #[arg(short, long, env = "ZIA_LISTEN_ADDR", default_value = "127.0.0.1:8080")]
  pub(crate) listen_addr: SocketAddr,
  #[arg(short, long, env = "ZIA_UPSTREAM")]
  pub(crate) upstream: Url,
  #[arg(short, long, env = "ZIA_PROXY")]
  pub(crate) proxy: Option<Url>,
}
