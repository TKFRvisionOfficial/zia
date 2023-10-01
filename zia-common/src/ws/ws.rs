use std::io::Result;

use anyhow::anyhow;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ws::{Event, Frame, Role};

/// WebSocket implementation for both client and server
pub struct WebSocket<IO> {
  /// it is a low-level abstraction that represents the underlying byte stream over which WebSocket messages are exchanged.
  pub io: IO,

  /// Maximum allowed payload length in bytes.
  pub max_payload_len: usize,

  role: Role,
  is_closed: bool,
}

impl<IO> WebSocket<IO> {
  #[inline]
  pub fn new(stream: IO, max_payload_len: usize, role: Role) -> Self {
    Self {
      io: stream,
      max_payload_len,
      role,
      is_closed: false,
    }
  }
}

impl<W: Unpin + AsyncWrite> WebSocket<W> {
  pub async fn send(&mut self, frame: Frame<'_>) -> anyhow::Result<()> {
    match self.role {
      Role::Server => frame.write_without_mask(&mut self.io).await?,
      Role::Client { masking } => {
        if masking {
          let mask = rand::random::<u32>().to_ne_bytes();
          frame.write_with_mask(&mut self.io, mask).await?;
        } else {
          frame.write_without_mask(&mut self.io).await?;
        }
      }
    }

    Ok(())
  }

  // TODO: implement close
  // pub async fn close<T>(mut self, reason: T) -> anyhow::Result<()>
  // where
  //   T: CloseReason,
  //   T::Bytes: AsRef<[u8]>,
  // {
  //   let frame = Frame {
  //     fin: true,
  //     opcode: 8,
  //     data: reason.to_bytes().as_ref(),
  //   };
  //
  //   self.send(frame).await?;
  //   self.flush().await?;
  //   Ok(())
  // }

  pub async fn flush(&mut self) -> anyhow::Result<()> {
    self.io.flush().await?;
    Ok(())
  }
}

// ------------------------------------------------------------------------

macro_rules! err { [$msg: expr] => { return Err(anyhow!($msg)) }; }

impl<R> WebSocket<R>
where
  R: Unpin + AsyncRead,
{
  /// reads [Event] from websocket stream.
  pub async fn recv(&mut self) -> anyhow::Result<Event> {
    if self.is_closed {
      return Err(std::io::Error::new(
        std::io::ErrorKind::NotConnected,
        "read after close",
      ))?;
    }
    let event = self.recv_event().await;
    if let Ok(Event::Close { .. }) | Err(..) = event {
      self.is_closed = true;
    }
    event
  }

  // ### WebSocket Frame Header
  //
  // ```txt
  //  0                   1                   2                   3
  //  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
  // +-+-+-+-+-------+-+-------------+-------------------------------+
  // |F|R|R|R| opcode|M| Payload len |    Extended payload length    |
  // |I|S|S|S|  (4)  |A|     (7)     |             (16/64)           |
  // |N|V|V|V|       |S|             |   (if payload len==126/127)   |
  // | |1|2|3|       |K|             |                               |
  // +-+-+-+-+-------+-+-------------+ - - - - - - - - - - - - - - - +
  // |     Extended payload length continued, if payload len == 127  |
  // + - - - - - - - - - - - - - - - +-------------------------------+
  // |                               |Masking-key, if MASK set to 1  |
  // +-------------------------------+-------------------------------+
  // | Masking-key (continued)       |          Payload Data         |
  // +-------------------------------- - - - - - - - - - - - - - - - +
  // :                     Payload Data continued ...                :
  // + - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - +
  // |                     Payload Data continued ...                |
  // +---------------------------------------------------------------+
  // ```
  /// reads [Event] from websocket stream.
  pub async fn recv_event(&mut self) -> anyhow::Result<Event> {
    let mut buf = [0u8; 2];
    self.io.read_exact(&mut buf).await?;

    let [b1, b2] = buf;

    let fin = b1 & 0b1000_0000 != 0;
    let rsv = b1 & 0b111_0000;
    let opcode = b1 & 0b1111;
    let len = (b2 & 0b111_1111) as usize;

    // Defines whether the "Payload data" is masked.  If set to 1, a
    // masking key is present in masking-key, and this is used to unmask
    // the "Payload data" as per [Section 5.3](https://datatracker.ietf.org/doc/html/rfc6455#section-5.3).  All frames sent from
    // client to server have this bit set to 1.
    let is_masked = b2 & 0b_1000_0000 != 0;

    if rsv != 0 {
      // MUST be `0` unless an extension is negotiated that defines meanings
      // for non-zero values.  If a nonzero value is received and none of
      // the negotiated extensions defines the meaning of such a nonzero
      // value, the receiving endpoint MUST _Fail the WebSocket Connection_.
      err!("reserve bit must be `0`");
    }

    // A client MUST mask all frames that it sends to the server. (Note
    // that masking is done whether or not the WebSocket Protocol is running
    // over TLS.)  The server MUST close the connection upon receiving a
    // frame that is not masked.
    //
    // A server MUST NOT mask any frames that it sends to the client.
    if let Role::Server = self.role {
      // TODO: disabled, to allow unmasked client frames
      // if !is_masked {
      //   err!("expected masked frame");
      // }
    } else if is_masked {
      err!("expected unmasked frame");
    }

    // 3-7 are reserved for further non-control frames.
    if opcode >= 8 {
      if !fin {
        err!("control frame must not be fragmented");
      }
      if len > 125 {
        err!("control frame must have a payload length of 125 bytes or less");
      }
      let msg = self.read_payload(is_masked, len).await?;
      match opcode {
        8 => on_close(&msg),
        // 9 => Ok(Event::Ping(msg)),
        // 10 => Ok(Event::Pong(msg)),
        // 11-15 are reserved for further control frames
        _ => err!("unknown opcode"),
      }
    } else {
      match (opcode, fin) {
        (2, true) => {}
        _ => err!("invalid data frame"),
      };
      let len = match len {
        126 => self.io.read_u16().await? as usize,
        127 => self.io.read_u64().await? as usize,
        len => len,
      };
      if len > self.max_payload_len {
        err!("payload too large");
      }
      let data = self.read_payload(is_masked, len).await?;
      Ok(Event::Data(data))
    }
  }

  async fn read_payload(&mut self, masked: bool, len: usize) -> Result<Box<[u8]>> {
    let mut data = vec![0; len].into_boxed_slice();
    match self.role {
      Role::Server => {
        if masked {
          let mut mask = [0u8; 4];
          self.io.read_exact(&mut mask).await?;
          self.io.read_exact(&mut data).await?;
          // TODO: Use SIMD wherever possible for best performance
          for i in 0..data.len() {
            data[i] ^= mask[i & 3];
          }
        } else {
          self.io.read_exact(&mut data).await?;
        }
      }
      Role::Client { .. } => {
        self.io.read_exact(&mut data).await?;
      }
    }
    Ok(data)
  }
}

/// - If there is a body, the first two bytes of the body MUST be a 2-byte unsigned integer (in network byte order: Big Endian)
///   representing a status code with value /code/ defined in [Section 7.4](https:///datatracker.ietf.org/doc/html/rfc6455#section-7.4).
///   Following the 2-byte integer,
///
/// - The application MUST NOT send any more data frames after sending a `Close` frame.
///
/// - If an endpoint receives a Close frame and did not previously send a
///   Close frame, the endpoint MUST send a Close frame in response.  (When
///   sending a Close frame in response, the endpoint typically echos the
///   status code it received.)  It SHOULD do so as soon as practical.  An
///   endpoint MAY delay sending a Close frame until its current message is
///   sent
///
/// - After both sending and receiving a Close message, an endpoint
///   considers the WebSocket connection closed and MUST close the
///   underlying TCP connection.
fn on_close(msg: &[u8]) -> anyhow::Result<Event> {
  let code = msg
    .get(..2)
    .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
    .unwrap_or(1000);

  match code {
    1000..=1003 | 1007..=1011 | 1015 | 3000..=3999 | 4000..=4999 => {
      match msg.get(2..).map(|data| String::from_utf8(data.to_vec())) {
        Some(Ok(msg)) => Ok(Event::Close {
          code: code.into(),
          reason: msg.into_boxed_str(),
        }),
        None => Ok(Event::Close {
          code: code.into(),
          reason: "".into(),
        }),
        Some(Err(_)) => Err(anyhow!("invalid utf-8 payload")),
      }
    }
    _ => Err(anyhow!("invalid close code")),
  }
}