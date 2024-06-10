use anyhow::Result;
use odd::Wk;
use roaring::RoaringBitmap;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::future::Future;
use std::io::Write;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tracing::{debug, info};

pub mod odd;
pub mod processor;
pub mod transport;

pub trait Conn: AsyncRead + AsyncWrite + Send + Unpin {}

impl Conn for TcpStream {}

#[trait_variant::make(Send)]
pub trait TransportConn<T: Conn> {
    async fn new_conn(&self) -> Result<T>;
}

#[trait_variant::make]
pub trait Processor<C: Conn, S: Display> {
    async fn turn(&self, conn: Rc<RefCell<C>>) -> Result<S>;
}

pub struct Pressure<T, P, C, S>
where
    C: Conn,
    S: Display,
    T: TransportConn<C>,
    P: Processor<C, S>,
{
    rt: Arc<Runtime>,
    transport: T,
    processor: P,
    max: u32,
    conn: PhantomData<C>,
    status: PhantomData<S>,
}

impl<T, P, C, S> Pressure<T, P, C, S>
where
    C: Conn,
    S: Display,
    T: TransportConn<C>,
    P: Processor<C, S>,
{
    pub fn new(rt: Arc<Runtime>, transport: T, processor: P, max: Option<u32>) -> Self {
        Self {
            rt,
            transport,
            processor,
            max: max.unwrap_or(65535),
            conn: Default::default(),
            status: Default::default(),
        }
    }

    pub fn run(self, _collect: Box<dyn Write>) {
        let mut connections = HashMap::new();
        let mut pinned_futures = HashMap::new();
        let mut wake_run = HashMap::new();
        let bitmap = Arc::new(Mutex::new(RoaringBitmap::new()));
        let mut un_use_seq = (0..=self.max).collect::<BTreeSet<u32>>();
        let mut all_conn = 0;
        loop {
            info!("all conn{all_conn}, now conn{:}", connections.len());
            //sleep(Duration::from_secs(2));
            //connections.len()
            if connections.len() < self.max as usize {
                self.rt.block_on(async {
                    match self.transport.new_conn().await {
                        Ok(conn) => {
                            let seq = *un_use_seq.iter().next().unwrap();
                            un_use_seq.take(&seq);
                            bitmap.lock().unwrap().insert(seq);
                            connections.insert(seq, Rc::new(RefCell::new(conn)));
                            wake_run.insert(
                                seq,
                                futures::task::waker(Arc::new(Wk::new(bitmap.clone(), seq))),
                            );
                            all_conn += 1;
                        }
                        Err(_) => {
                            //TODO
                        }
                    };
                });
            }
            let mut conn_loop_iter = 0;
            loop {
                conn_loop_iter += 1;
                debug!("run loop iter exec num{conn_loop_iter}");
                let iter: Vec<_> = {
                    let mut map = bitmap.lock().unwrap();
                    let iter = map.clone().into_iter();
                    let min = map.min().unwrap_or_default();
                    let max = map.max().unwrap_or_default();
                    map.remove_range(min..=max);

                    // DEBUG
                    iter.collect()
                };
                if conn_loop_iter == 1 {
                    info!(
                        "get all wake seq len {:?} fist {:?}, last {:?}",
                        iter.len(),
                        iter.first(),
                        iter.last()
                    );
                }
                for seq in iter {
                    let waker = wake_run.get(&seq).unwrap();
                    let mut cx = Context::from_waker(waker);
                    let poll_result = {
                        let pinned = match pinned_futures.get_mut(&seq) {
                            Some(pinned) => {
                                debug!("use pinned future");
                                pinned
                            }
                            None => {
                                debug!("new pinned {seq}");
                                let conn = connections.get(&seq).unwrap().clone();
                                let pinned_owner = Box::pin(self.processor.turn(conn));
                                pinned_futures.insert(seq, pinned_owner);
                                pinned_futures.get_mut(&seq).unwrap()
                            }
                        };
                        debug!("start poll seq {seq}");
                        pinned.as_mut().poll(&mut cx)
                    };
                    debug!("end poll");
                    //debug!("get result {:?}",&poll_result);
                    match poll_result {
                        Poll::Ready(Ok(_)) => {
                            debug!("finish turn {seq}");
                            pinned_futures.remove(&seq);
                            waker.wake_by_ref();
                        }
                        Poll::Ready(Err(e)) => {
                            debug!("read or write err {e}");
                            pinned_futures.remove(&seq);
                            wake_run.remove(&seq);
                            connections.remove(&seq);
                            un_use_seq.insert(seq);
                        }
                        Poll::Pending => {}
                    }
                }
                let empty = { bitmap.lock().unwrap().is_empty() };
                if empty {
                    break;
                }
            }
            info!("one turn of loop num {conn_loop_iter}");
        }
    }
}

#[cfg(test)]
mod test {
    use crate::transport_layer::processor::{test::http_server, Echo, Http1Handle};
    use crate::transport_layer::transport::TcpSteamMaker;
    use crate::transport_layer::Pressure;
    use bytes::Bytes;
    use http_body_util::Empty;
    use log::info;

    use crate::transport_layer::processor::test::tcp_echo_process_listener;
    use std::env;
    use std::io::stdout;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    #[test]
    fn pressure_echo() {
        env::set_var("RUST_BACKTRACE", "1");

        tracing_subscriber::fmt()
            .with_line_number(true)
            .with_max_level(tracing::Level::INFO)
            .init();
        info!("test out");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.spawn(tcp_echo_process_listener());

        let transport_conn = TcpSteamMaker::new("127.0.0.1:8080");
        let processor = Echo::new();
        let pressure = Pressure::new(Arc::new(rt), transport_conn, processor, None);
        pressure.run(Box::new(stdout()))
    }

    #[test]
    fn http_test() {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
        info!("test out");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
        let listener = rt.block_on(async { TcpListener::bind(addr).await.unwrap() });
        rt.spawn(http_server(listener));

        let transport_conn = TcpSteamMaker::new("127.0.0.1:3000");
        let processor = Http1Handle::new("http://127.0.0.1:3000", Empty::<Bytes>::new()).unwrap();
        let pressure = Pressure::new(Arc::new(rt), transport_conn, processor, None);
        pressure.run(Box::new(stdout()))
    }
}
