use super::{Instruct, Statistics, Stop, TransportConn};
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::ops::Add;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
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
    fn stop<T>(&mut self, _status: &HashMap<u32, Vec<Statistics<T>>>) -> bool
    where
        T: Instruct,
    {
        self.load(Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct TimeOutStop {
    pub timeout: SystemTime,
}

impl Stop for TimeOutStop {
    fn stop<T>(&mut self, _status: &HashMap<u32, Vec<Statistics<T>>>) -> bool
    where
        T: Instruct,
    {
        SystemTime::now() > self.timeout
    }
}

impl TimeOutStop {
    pub fn new(duration: Duration) -> Self {
        Self {
            timeout: SystemTime::now().add(duration),
        }
    }
}
