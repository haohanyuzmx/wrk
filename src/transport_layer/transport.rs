use super::{Instruct, Statistics, Stop, TransportConn};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use tokio::net::TcpStream;

pub struct TcpSteamMaker {
    addr: SocketAddr,
}

impl TcpSteamMaker {
    pub fn new<A: ToSocketAddrs>(addr: A) -> Self {
        Self {
            addr: addr.to_socket_addrs().unwrap().next().unwrap(),
        }
    }
}

impl TransportConn<TcpStream> for TcpSteamMaker {
    async fn new_conn(&self) -> anyhow::Result<TcpStream> {
        Ok(TcpStream::connect(self.addr).await?)
    }
}

impl Stop for Arc<AtomicBool> {
    fn stop<T>(&mut self, status: &Statistics<T>) -> bool
    where
        T: Instruct
    {
        self.load(Relaxed)
    }
}
