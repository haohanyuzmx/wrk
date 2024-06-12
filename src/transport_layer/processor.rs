use super::odd::{Empty, WarpConn, Wk};
use crate::transport_layer::{Instruct, Processor};
use anyhow::anyhow;
use http_body_util::{combinators::Collect, BodyExt};
use hyper::body::{Body, Incoming};
use hyper::client::conn::http1 as client_http1;
use hyper::client::conn::http1::Connection;
use hyper::{Request, Response, Uri};
use hyper_util::rt::TokioIo;
use pin_project::pin_project;
use roaring::RoaringBitmap;
use std::cell::RefCell;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::future::Future;
use std::pin::{pin, Pin};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::Poll::{Pending, Ready};
use std::task::{ready, Context, Poll, Wake, Waker};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

#[derive(Default)]
pub struct Echo {}

impl Processor<TcpStream, Empty> for Echo {
    #[allow(clippy::await_holding_refcell_ref)]
    async fn turn(&self, conn: Rc<RefCell<TcpStream>>) -> anyhow::Result<Empty> {
        debug!("run turn");
        let mut conn_mut = conn.borrow_mut();
        conn_mut.write_all(b"hello").await?;
        debug!("process write success");
        // let mut buffer = Vec::new();
        // conn.read_to_end(&mut buffer).await?;
        let mut buf = vec![0; 1024];
        let num = conn_mut.read(&mut buf).await?;
        debug!(
            "get result {}",
            String::from_utf8(buf.into_iter().take(num).collect()).unwrap()
        );
        Ok(Empty)
    }
}

pub struct Http1Handle<T> {
    //uri:String,
    request: Request<T>,
}

impl<T: Clone + Body> Http1Handle<T> {
    pub fn new<S: ToString>(uri: S, body: T) -> anyhow::Result<Self> {
        let uri_string = uri.to_string();
        let uri_ = Uri::try_from(uri_string)?;
        let host = uri_
            .host()
            .ok_or_else(|| anyhow!("can't parse host"))?
            .to_string();
        Ok(Self {
            request: Request::builder()
                .uri(uri_)
                .header("Host", host)
                .body(body)?,
        })
    }
}

#[derive(Debug, Default)]
pub struct H1Detail {
    pub http_code: u16,
}

impl Display for H1Detail {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(self, f)
    }
}

impl Instruct for H1Detail {}

impl<T> Processor<TcpStream, H1Detail> for Http1Handle<T>
where
    T: Clone + Body + 'static,
    <T as Body>::Data: Send,
    <T as Body>::Error: Error + Send + Sync,
{
    async fn turn(&self, conn: Rc<RefCell<TcpStream>>) -> anyhow::Result<H1Detail> {
        let tokio_io = TokioIo::new(WarpConn(conn));
        let (mut request_sender, connection) = client_http1::handshake(tokio_io).await?;
        let req = self.request.clone();
        let status = H1Call::new(connection, Box::pin(request_sender.send_request(req))).await?;
        Ok(H1Detail { http_code: status })
    }
}

struct WKWithMark {
    mark: Waker,
    wk: Waker,
}

impl WKWithMark {
    fn new(mark: Waker, wk: Waker) -> Self {
        Self { mark, wk }
    }
}

impl Wake for WKWithMark {
    fn wake(self: Arc<Self>) {
        self.mark.wake_by_ref();
        self.wk.wake_by_ref();
    }
}

#[pin_project]
struct H1Call<T, B>
where
    T: hyper::rt::Read + hyper::rt::Write,
    B: Body + 'static,
{
    status: Option<Arc<Mutex<RoaringBitmap>>>,
    #[pin]
    conn: Connection<T, B>,
    request: Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>>>>,
    incoming: Option<Pin<Box<Collect<Incoming>>>>,
    result: u16,
    // cx: [Option<Waker>; 2],
}

impl<T, B> H1Call<T, B>
where
    T: hyper::rt::Read + hyper::rt::Write,
    B: Body + 'static,
{
    fn new(
        conn: Connection<T, B>,
        request: Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>>>>,
    ) -> Self {
        Self {
            status: None,
            conn,
            request,
            incoming: None,
            result: 0,
            // cx: [None; 2],
        }
    }
}

impl<T, B> Future for H1Call<T, B>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin,
    B: Clone + Body + 'static,
    <B as Body>::Data: Send,
    <B as Body>::Error: Error + Send + Sync,
{
    type Output = anyhow::Result<u16>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let is_first = {
            if this.status.is_none() {
                this.status
                    .replace(Arc::new(Mutex::new(RoaringBitmap::new())));
                true
            } else {
                false
            }
        };
        let waker = this.status.as_mut().unwrap();
        let notify: HashSet<u32> = {
            let mut map = waker.lock().unwrap();
            let iter = map.clone().into_iter();
            let min = map.min().unwrap_or_default();
            let max = map.max().unwrap_or_default();
            map.remove_range(min..=max);
            iter.collect()
        };

        let cx_mark0 = futures::task::waker(Arc::new(Wk::new(waker.clone(), 0)));
        let cx_mark1 = futures::task::waker(Arc::new(Wk::new(waker.clone(), 1)));
        let cx_wk0 = Arc::new(WKWithMark::new(cx_mark0, cx.waker().clone())).into();
        let cx_wk1 = Arc::new(WKWithMark::new(cx_mark1, cx.waker().clone())).into();
        // 尽管入参引用，但是wake trait变成waker的时候调用了forget，这里函数结束drop cx_wk没问题，但是如果用闭包封装函数，会导致编译器不高兴过不了编译
        let mut wk0 = Context::from_waker(&cx_wk0);
        let mut wk1 = Context::from_waker(&cx_wk1);

        if is_first || notify.contains(&0) {
            //this.cx[0].replace(cx_wk0);
            let conn_result = this.conn.poll(&mut wk0);
            if conn_result.is_ready() {
                return Ready(Err(anyhow!("connection return {:?}", conn_result)));
            }
        }
        if !is_first && !notify.contains(&1) {
            return Pending;
        }
        //this.cx[0].replace(cx_wk1);
        let future = if this.incoming.is_none() {
            let req_result = ready!(this.request.as_mut().poll(&mut wk1));
            if req_result.is_err() {
                return Ready(Err(anyhow!("request err {:?}", req_result)));
            }
            let resp = req_result.unwrap();
            let (header, body) = resp.into_parts();
            *this.result = header.status.into();
            this.incoming.replace(Box::pin(body.collect()));
            this.incoming.as_mut().unwrap().as_mut()
        } else {
            this.incoming.as_mut().unwrap().as_mut()
        };
        let result = ready!(future.poll(&mut wk1));
        if result.is_err() {
            Ready(Err(anyhow!("get body result err {:?}", result)))
        } else {
            Ready(Ok(*this.result))
        }
    }
}

#[cfg(test)]
pub mod test {
    use crate::transport_layer::odd::WarpConn;
    use crate::transport_layer::processor::{H1Call, Http1Handle};
    use crate::transport_layer::Processor;
    use anyhow::Error;
    use http_body_util::{Empty, Full};
    use hyper::body::Bytes;
    use hyper::service::service_fn;
    use hyper::{
        client::conn::http1 as client_http1, server::conn::http1 as server_http1, StatusCode,
    };
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use std::cell::RefCell;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::rc::Rc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::spawn;
    use tracing::debug;

    #[tokio::test]
    async fn http_conn() {
        // tracing_subscriber::fmt()
        //     .with_max_level(tracing::Level::INFO)
        //     .init();
        let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
        let listener = TcpListener::bind(addr).await.unwrap();
        spawn(muti_pack_server(listener));
        let stream = TcpStream::connect("127.0.0.1:8080").await.unwrap();
        let http_handle = Http1Handle::new("http://127.0.0.1:8080", Empty::<Bytes>::new()).unwrap();
        let value = RefCell::new(stream);
        let value = Rc::new(value);
        for _ in 0..10 {
            let _ = http_handle.turn(value.clone()).await.unwrap();
        }
    }

    #[allow(dead_code)]
    //#[tokio::test(flavor = "multi_thread")]
    async fn tokio() {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
        let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
        let listener = TcpListener::bind(addr).await.unwrap();
        spawn(muti_pack_server(listener));

        let stream = TcpStream::connect("127.0.0.1:8080").await.unwrap();
        let c = Rc::new(RefCell::new(stream));
        let wc = WarpConn(c);
        let (mut request_sender, connection) =
            client_http1::handshake(TokioIo::new(wc)).await.unwrap();
        // spawn(async move {
        //     if let Err(e) = connection.await {
        //         eprintln!("Error in connection: {}", e);
        //     }
        // });
        let request = Request::builder()
            .uri("http://127.0.0.1:8080")
            .header("Host", "127.0.0.1")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let fu = request_sender.send_request(request);
        let call = H1Call::new(connection, Box::pin(fu));
        let result = call.await;
        // let response = request_sender.send_request(request).await.unwrap();
        //
        // let b = response.into_body().collect().await.unwrap().to_bytes();
        // dbg!(b.len());
        assert_eq!(result.unwrap(), StatusCode::OK.as_u16());
    }

    pub async fn tcp_echo_process_listener() {
        let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
        let mut num = 0;
        loop {
            num += 1;
            let (mut conn, _) = listener.accept().await.unwrap();
            //debug!("conn num {num}");
            spawn(async move {
                //debug!("start run");
                let mut turn = 0;
                loop {
                    turn += 1;
                    debug!("conn {num}, server start");
                    let mut buf = vec![0; 1024];
                    let read_num = conn.read(&mut buf).await?;
                    let str = String::from_utf8_lossy(&buf[0..read_num]);
                    debug!("read_num {read_num},read_conn ,{:}", &str);
                    let write_num = conn.write(&buf[0..read_num]).await?;
                    debug!("conn_num {num},conn_turn {turn},write_num {write_num}");
                }
                #[allow(unreachable_code)]
                Ok::<(), Error>(())
            });
        }
    }

    pub async fn http_server(listener: TcpListener) {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);

            tokio::task::spawn(async move {
                if let Err(err) = server_http1::Builder::new()
                    .serve_connection(io, service_fn(hello))
                    .await
                {
                    println!("Error serving connection: {:?}", err);
                }
            });
        }
    }

    async fn hello(_: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
        Ok(Response::new(Full::new(Bytes::from("Hello, World!"))))
    }

    pub async fn muti_pack_server(lis: TcpListener) {
        loop {
            let (stream, _) = lis.accept().await.unwrap();
            let io = TokioIo::new(stream);

            tokio::task::spawn(async move {
                if let Err(err) = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|_: Request<hyper::body::Incoming>| async {
                            let big_pack: Vec<u8> = vec![1; 16384];
                            Ok::<Response<http_body_util::Full<bytes::Bytes>>, Infallible>(
                                Response::new(Full::<Bytes>::from(big_pack)),
                            )
                        }),
                    )
                    .await
                {
                    println!("Error serving connection: {:?}", err);
                }
            });
        }
    }
}
