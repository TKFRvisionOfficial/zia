use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::{datagram_buffer, MAX_DATAGRAM_SIZE};
use tokio::io::{AsyncWrite, WriteHalf};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::{error, warn};

use crate::pool::{Pool, PoolEntry};
use crate::ws::{Message, WebSocket};

pub struct WriteConnection<W> {
  write: WebSocket<WriteHalf<W>>,
  buf: Box<[u8; MAX_DATAGRAM_SIZE]>,
}

impl<W: AsyncWrite> WriteConnection<W> {
  pub fn new(write: WebSocket<WriteHalf<W>>) -> Self {
    Self {
      buf: datagram_buffer(),
      write,
    }
  }

  async fn flush(&mut self, size: usize) -> anyhow::Result<()> {
    assert!(size <= MAX_DATAGRAM_SIZE);

    let message = Message::Binary(&self.buf[..size]);
    self.write.send(message).await?;

    Ok(())
  }
}

impl<W> PoolEntry for WriteConnection<W> {
  fn is_closed(&self) -> bool {
    self.write.is_closed()
    // TODO: open new connection on client - maybe fancy login in "abstract" pool
  }
}

pub struct WritePool<W> {
  socket: Arc<UdpSocket>,
  pool: Pool<WriteConnection<W>>,
  addr: Arc<RwLock<Option<SocketAddr>>>,
}

impl<W: AsyncWrite + Send + 'static> WritePool<W> {
  pub fn new(socket: Arc<UdpSocket>, addr: Arc<RwLock<Option<SocketAddr>>>) -> Self {
    Self {
      socket,
      pool: Pool::new(),
      addr,
    }
  }

  async fn update_addr(&self, addr: SocketAddr) {
    let is_outdated = self
      .addr
      .read()
      .await
      .map(|last_addr| last_addr != addr)
      .unwrap_or(true);

    if is_outdated {
      *(self.addr.write().await) = Some(addr);
    }
  }

  pub async fn push(&self, conn: WriteConnection<W>) {
    self.pool.push(conn);
  }

  pub async fn execute(&self) -> anyhow::Result<()> {
    loop {
      let conn = self.pool.acquire().await;

      // TODO:
      // maybe just block until it is not empty anymore
      // .revc() in self.pool.acquire() would be blocking
      // until a connection becomes available, therefore
      // this would be appropriate
      let mut conn = match conn {
        Some(conn) => conn,
        None => {
          warn!("Write pool is empty, waiting 1s");
          tokio::time::sleep(Duration::from_secs(1)).await;
          continue;
        }
      };

      if conn.is_closed() {
        continue;
      }

      // read from udp socket and save to buf of selected conn
      let (read, addr) = self.socket.recv_from(conn.buf.as_mut()).await.unwrap();

      self.update_addr(addr).await;

      // flush buf of conn asynchronously to read again from udp socket in parallel
      tokio::spawn(async move {
        if let Err(err) = conn.flush(read).await {
          error!("Unable to flush websocket buf: {:?}", err);
        }
      });
    }
  }
}
