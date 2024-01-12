use crate::transport_layer::TransportConn;
use std::net::{SocketAddr, ToSocketAddrs};
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
