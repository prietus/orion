//! Plain or TLS TCP stream (rustls + the system certificate verifier).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

use orion_core::{Error, Result};

#[derive(Debug)]
pub enum ImapStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}


pub async fn tls_wrap(tcp: TcpStream, host: &str) -> Result<ImapStream> {
    let name = ServerName::try_from(host.to_string()).map_err(|e| Error::Network(format!("invalid host name: {e}")))?;
    let tls = TlsConnector::from(crate::tls::client_config()?)
        .connect(name, tcp)
        .await
        .map_err(|e| Error::Network(format!("TLS with {host}: {e}")))?;
    Ok(ImapStream::Tls(Box::new(tls)))
}

impl AsyncRead for ImapStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ImapStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            ImapStream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ImapStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            ImapStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            ImapStream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ImapStream::Plain(s) => Pin::new(s).poll_flush(cx),
            ImapStream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ImapStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            ImapStream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
