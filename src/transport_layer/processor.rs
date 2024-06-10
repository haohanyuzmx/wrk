use super::odd::{Empty, WarpConn};
use crate::transport_layer::Processor;
use anyhow::anyhow;
use hyper::body::Body;
use hyper::client::conn::http1 as client_http1;
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use std::cell::RefCell;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::rc::Rc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::select;
use tracing::debug;

pub struct Echo {}

impl Default for Echo {
    fn default() -> Self {
        Self::new()
    }
}

impl Echo {
    pub fn new() -> Self {
        Self {}
    }
}

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

// TODO: 扩展processor，让Pressure负责conn的pull
impl<T> Processor<TcpStream, H1Detail> for Http1Handle<T>
where
    T: Clone + Body + 'static,
    <T as Body>::Data: Send,
    <T as Body>::Error: Error + Send + Sync,
{
    async fn turn(&self, conn: Rc<RefCell<TcpStream>>) -> anyhow::Result<H1Detail> {
        let tokio_io = TokioIo::new(WarpConn(conn));
        let (mut request_sender, connection) = client_http1::handshake(tokio_io).await.unwrap();
        let req = self.request.clone();

        select! {
            // 理论来说connection不会await成功，只有Shutdown和upgrade会返回
            conn_err=connection=>{
                conn_err?;
                Err(anyhow!("shutdown or upgrade"))
            }
            response=request_sender.send_request(req)=>{
                // TODO: body 是income类型，会不会污染后续从conn读
                Ok(H1Detail{http_code:response?.status().into()})
           }
        }
    }
}

#[cfg(test)]
pub mod test {

    use crate::transport_layer::processor::{Http1Handle};
    use crate::transport_layer::Processor;
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::body::Bytes;
    use hyper::service::service_fn;
    use hyper::{client::conn::http1 as client_http1, server::conn::http1 as server_http1};
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use std::cell::RefCell;
    use std::convert::Infallible;
    use std::rc::Rc;

    use crate::transport_layer::odd::WarpConn;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::spawn;
    use tracing::debug;

    #[tokio::test]
    async fn http_conn() {
        let stream = TcpStream::connect("127.0.0.1:8080").await.unwrap();
        //let (mut request_sender, connection) = client_http1::handshake(TokioIo::new(stream)).await.unwrap();
        // spawn(async move {
        //     if let Err(e) = connection.await {
        //         eprintln!("Error in connection: {}", e);
        //     }
        // });
        let http_handle = Http1Handle::new("http://127.0.0.1:8080", Empty::<Bytes>::new()).unwrap();
        let _ = http_handle
            .turn(Rc::new(RefCell::new(stream)))
            .await
            .unwrap();

        //let response = request_sender.send_request(request).await.unwrap();
        // assert_eq!(response.status(), StatusCode::OK);

        // let request = Request::builder()
        //     .header("Host", "example.com")
        //     .method("GET")
        //     .body(Empty::<Bytes>::new()).unwrap();

        // let response = request_sender.send_request(request).await.unwrap();
        // assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn tokio() {
        let stream = TcpStream::connect("127.0.0.1:8080").await.unwrap();
        let c = Rc::new(RefCell::new(stream));
        let wc = WarpConn(c);
        let (mut request_sender, connection) =
            client_http1::handshake(TokioIo::new(wc)).await.unwrap();
        spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("Error in connection: {}", e);
            }
        });
        let request = Request::builder()
            .uri("http://127.0.0.1:8080")
            .header("Host", "127.0.0.1")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let response = request_sender.send_request(request).await.unwrap();

        dbg!(response.into_body().frame().await);
        //assert_eq!(response.status(), StatusCode::OK);
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
                    let read_num = conn.read(&mut buf).await.unwrap();
                    let str = String::from_utf8_lossy(&buf[0..read_num]);
                    debug!("read_num {read_num},read_conn ,{:}", &str);
                    let write_num = conn.write(&buf[0..read_num]).await.unwrap();
                    debug!("conn_num {num},conn_turn {turn},write_num {write_num}");
                }
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
}
