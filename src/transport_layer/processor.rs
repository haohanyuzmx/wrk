use crate::transport_layer::Processor;
use hyper::body::Body;
use hyper::client::conn::http1 as client_http1;
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use std::cell::RefCell;
use std::error::Error;
use std::io;
use std::io::IoSlice;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use anyhow::anyhow;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
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

impl Processor<TcpStream> for Echo {
    async fn turn(&self, conn: Arc<RefCell<TcpStream>>) -> anyhow::Result<()> {
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
        Ok(())
    }
}

pub struct HttpHandle<T> {
    //uri:String,
    request: Request<T>,
}

impl<T: Clone + Body> HttpHandle<T> {
    pub fn new<S: ToString>(uri: S, body: T) -> anyhow::Result<Self> {
        let uri_string = uri.to_string();
        let uri_ = Uri::try_from(uri_string)?;
        let host=uri_.host().ok_or_else(||{anyhow!("can't parse host")})?.to_string();
        Ok(Self {
            request: Request::builder().uri(uri_).header("Host",host).body(body)?,
        })
    }
}

struct WarpConn(Arc<RefCell<TcpStream>>);

unsafe impl Send for WarpConn {}

macro_rules! pinc {
    ($self:ident) => {
        Pin::new($self.get_mut().0.borrow_mut().deref_mut())
    };
}
impl AsyncRead for WarpConn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        pinc!(self).poll_read(cx, buf)
    }
}

impl AsyncWrite for WarpConn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        pinc!(self).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        pinc!(self).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        pinc!(self).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        pinc!(self).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.0.borrow().is_write_vectored()
    }
}

impl<T> Processor<TcpStream> for HttpHandle<T>
where
    T: Clone + Body + 'static,
    <T as Body>::Data: Send,
    <T as Body>::Error: Error + Send + Sync,
{
    async fn turn(&self, conn: Arc<RefCell<TcpStream>>) -> anyhow::Result<()> {
        let tokio_io = TokioIo::new(WarpConn(conn));
        let (mut request_sender, connection) = client_http1::handshake(tokio_io).await.unwrap();
        let req = self.request.clone();

        select! {
            conn_err=connection=>{
                Ok(conn_err?)
            }
            response=request_sender.send_request(req)=>{
                if response?.status()!=200{
                    // TODO:
                }
                Ok(())
           }
        }
    }
}

#[cfg(test)]
pub mod test {
    
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::body::{Bytes};
    use hyper::service::service_fn;
    use hyper::{client::conn::http1 as client_http1, server::conn::http1 as server_http1};
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use std::cell::RefCell;
    use std::convert::Infallible;
    
    
    use std::sync::Arc;

    use crate::transport_layer::processor::{HttpHandle, WarpConn};
    use crate::transport_layer::Processor;
    
    use tokio::net::{TcpListener, TcpStream};
    use tokio::{spawn};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        let http_handle = HttpHandle::new("http://127.0.0.1:8080", Empty::<Bytes>::new()).unwrap();
        http_handle
            .turn(Arc::new(RefCell::new(stream)))
            .await
            .unwrap()

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
        let c = Arc::new(RefCell::new(stream));
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
            .header("Host","127.0.0.1")
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
