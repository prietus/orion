//! Blocking TCP/TLS connections shared by the POP3 and SMTP clients.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConnection, StreamOwned};

use orion_core::{Error, Result};

pub(crate) enum Conn {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
    /// Placeholder while upgrading with STLS.
    None,
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(buf),
            Conn::Tls(s) => s.read(buf),
            Conn::None => Ok(0),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
            Conn::None => Err(std::io::ErrorKind::NotConnected.into()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
            Conn::None => Ok(()),
        }
    }
}


fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

pub(crate) fn tls(tcp: TcpStream, host: &str) -> Result<StreamOwned<ClientConnection, TcpStream>> {
    let name = ServerName::try_from(host.to_string()).map_err(|e| net(format!("invalid host name: {e}")))?;
    let conn = ClientConnection::new(crate::tls::client_config()?, name).map_err(|e| net(format!("TLS with {host}: {e}")))?;
    Ok(StreamOwned::new(conn, tcp))
}

/// TCP connection with connect/read/write timeouts.
pub(crate) fn tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| net(format!("{host}:{port}: {e}")))?
        .next()
        .ok_or_else(|| net(format!("{host}: no address")))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(20)).map_err(|e| net(format!("{host}:{port}: {e}")))?;
    tcp.set_read_timeout(Some(timeout)).map_err(net)?;
    tcp.set_write_timeout(Some(timeout)).map_err(net)?;
    tcp.set_nodelay(true).ok();
    Ok(tcp)
}

/// Upgrades a plain connection after STARTTLS/STLS. Anything already
/// buffered would be plaintext smuggled before TLS (STARTTLS injection).
pub(crate) fn upgrade(io: &mut std::io::BufReader<Conn>, host: &str) -> Result<()> {
    if !io.buffer().is_empty() {
        return Err(net("data received before TLS started"));
    }
    let Conn::Plain(tcp) = std::mem::replace(io.get_mut(), Conn::None) else {
        return Err(net("STARTTLS on an encrypted connection"));
    };
    *io = std::io::BufReader::new(Conn::Tls(Box::new(tls(tcp, host)?)));
    Ok(())
}
