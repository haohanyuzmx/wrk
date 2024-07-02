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
use std::time;
use std::time::Duration;
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
pub trait Processor<C: Conn, S: Instruct> {
    async fn turn(&self, conn: Rc<RefCell<C>>) -> Result<S>;
}

pub trait Stop {
    fn stop<T>(&mut self, status: &Statistics<T>) -> bool
    where
        T: Instruct;
}

pub trait Instruct: Display + Default {
    // 0-1的归一化数值，指示压力状况
    fn instruct(&self) -> f64 {
        0f64
    }
}

// per conn?
#[derive(Default)]
pub struct Statistics<T: Instruct> {
    // pull间隔
    pull_interval: Vec<Duration>,
    // 每次turn完成花费的时间
    // 理论上这个的len就是turn的次数
    turn_interval: Vec<Duration>,
    // Processor 内输出的信息
    turn_collect: Vec<T>,
}

pub struct Pressure<T, P, C, S>
where
    C: Conn,
    S: Instruct,
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
    S: Instruct,
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
    pub fn run<Stp>(self, mut stop: Stp) -> Vec<Statistics<S>>
    where
        Stp: Stop,
    {
        let mut connections = HashMap::new();
        let mut pinned_futures = HashMap::new();
        let mut pinned_futures_start_time = HashMap::new();
        let mut pinned_poll_interval = HashMap::new();
        let mut wake_run = HashMap::new();
        let mut conn_sta = HashMap::new();
        let bitmap = Arc::new(Mutex::new(RoaringBitmap::new()));
        let mut un_use_seq = (0..=self.max).collect::<BTreeSet<u32>>();
        let mut all_conn = 0;
        let begin = time::SystemTime::now();
        loop {
            if stop.stop() {
                let mut all_connected: usize = 0;
                let mut all_turn = 0;
                let status: Vec<Statistics<S>> = conn_sta.into_values().flatten().collect();
                for sta in &status {
                    all_connected += 1;
                    all_turn += sta.turn_interval.len();
                }
                let cost = time::SystemTime::now().duration_since(begin).unwrap();
                //info!("all conn:{all_connected},all req:{all_turn},cost:{cost}");
                return status;
            }
            debug!("all conn{all_conn}, now conn{:}", connections.len());
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
                            conn_sta
                                .entry(seq)
                                .or_insert(Vec::new())
                                .push(Statistics::default());
                            all_conn += 1;
                        }
                        Err(_) => {
                            //TODO
                        }
                    };
                });
            }
            let mut conn_loop_iter = 0;
            let mut pre = vec![];
            loop {
                conn_loop_iter += 1;
                debug!("run loop",num=%conn_loop);
                let iter: Vec<_> = {
                    let mut map = bitmap.lock().unwrap();
                    let iter = map.clone().into_iter();
                    let min = map.min().unwrap_or_default();
                    let max = map.max().unwrap_or_default();
                    map.remove_range(min..=max);

                    // DEBUG
                    iter.collect()
                };
                for seq in iter {
                    let mut status = conn_sta.get_mut(&seq).unwrap().last_mut().unwrap();
                    let loop_start = time::SystemTime::now();
                    let interval_entry = pinned_poll_interval.entry(seq);
                    interval_entry
                        .and_modify(|last| {
                            let result = loop_start.duration_since(last.clone()).unwrap();
                            status.pull_interval.push(result);
                            *last = loop_start;
                        })
                        .or_insert(loop_start);

                    let waker = wake_run.get(&seq).unwrap();
                    let mut cx = Context::from_waker(waker);
                    let poll_result = {
                        let pinned = match pinned_futures.get_mut(&seq) {
                            Some(pinned) => {
                                debug!("use pinned future");
                                pinned
                            }
                            None => {
                                debug!("new pinned");
                                let conn = connections.get(&seq).unwrap().clone();
                                let pinned_owner = Box::pin(self.processor.turn(conn));
                                pinned_futures_start_time.insert(seq, loop_start.clone());
                                pinned_futures.insert(seq, pinned_owner);
                                pinned_futures.get_mut(&seq).unwrap()
                            }
                        };
                        debug!("start poll",seq=%seq);
                        pinned.as_mut().poll(&mut cx)
                    };
                    debug!("end poll");
                    let collect = |ins| {
                        let start = pinned_futures_start_time.get(seq).unwrap().clone();
                        let turn = loop_start.duration_since(start).unwrap();
                        status.turn_interval.push(turn);
                        if let Some(i) = ins {
                            status.turn_collect.push(i);
                        }
                    };
                    match poll_result {
                        Poll::Ready(Ok(ins)) => {
                            collect(Some(ins));
                            let pressure = ins.instruct();
                            pre.push(pressure);
                            debug!("finish turn",seq=%seq);
                            pinned_futures.remove(&seq);
                            waker.wake_by_ref();
                        }
                        Poll::Ready(Err(e)) => {
                            collect(None);
                            debug!("read or write err",err=%e);
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
            debug!("one turn of loop",num=%conn_loop_iter);
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
    use std::sync::atomic::AtomicBool;
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
        let processor = Echo::default();
        let pressure = Pressure::new(Arc::new(rt), transport_conn, processor, None);
        let a = Arc::new(AtomicBool::new(false));
        pressure.run(a);
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
        let a = Arc::new(AtomicBool::new(false));
        pressure.run(a);
    }
}
