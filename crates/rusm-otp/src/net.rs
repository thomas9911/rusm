use std::future::Future;
use std::io;
use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};

use crate::runtime::{Context, ProcessHandle, Runtime};

impl Runtime {
    /// Binds a TCP listener on `addr` and serves it **one process per
    /// connection**: every accepted socket runs `handler` as its own isolated
    /// rusm-otp process, so a slow or crashing connection can't affect the
    /// others. Returns the bound address (handy with port 0) and a handle to the
    /// acceptor process — kill it to stop listening.
    ///
    /// This is cheap because spawning is cheap: a connection costs one process,
    /// and the runtime mints those far faster than any OS TCP stack hands out
    /// sockets — so the connection ceiling is the OS, not RUSM.
    pub async fn listen<F, Fut>(
        &self,
        addr: impl ToSocketAddrs,
        handler: F,
    ) -> io::Result<(SocketAddr, ProcessHandle)>
    where
        F: Fn(Context, TcpStream) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        let rt = self.clone();
        let acceptor = self.spawn(move |_ctx| async move {
            // Killing the acceptor drops `listener`, which closes the port.
            while let Ok((stream, _peer)) = listener.accept().await {
                let handler = handler.clone();
                rt.spawn(move |ctx| handler(ctx, stream));
            }
        });
        Ok((local, acceptor))
    }

    /// Opens an outbound TCP connection.
    pub async fn connect(&self, addr: impl ToSocketAddrs) -> io::Result<TcpStream> {
        TcpStream::connect(addr).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn echoes_over_a_process_per_connection() {
        let rt = Runtime::new();
        let (addr, _acceptor) = rt
            .listen("127.0.0.1:0", |_ctx, mut stream| async move {
                let mut buf = [0u8; 5];
                stream.read_exact(&mut buf).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            })
            .await
            .unwrap();

        let mut client = rt.connect(addr).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_connection_is_its_own_live_process() {
        let rt = Runtime::new();
        let (addr, acceptor) = rt
            .listen("127.0.0.1:0", |mut ctx, _stream| async move {
                let _ = ctx.recv().await; // hold the connection open until killed
            })
            .await
            .unwrap();

        let mut clients = Vec::new();
        for _ in 0..3 {
            clients.push(rt.connect(addr).await.unwrap());
        }
        // 3 connection handlers + the acceptor.
        while rt.process_count() < 4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(rt.process_count(), 4);

        drop(clients);
        acceptor.kill();
    }

    #[tokio::test]
    async fn killing_the_acceptor_closes_the_port() {
        let rt = Runtime::new();
        let (addr, acceptor) = rt
            .listen("127.0.0.1:0", |_ctx, _stream| async {})
            .await
            .unwrap();
        acceptor.kill();
        acceptor.join().await; // listener is dropped once the acceptor is gone
        assert!(rt.connect(addr).await.is_err());
    }
}
