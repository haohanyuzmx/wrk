use futures::task::ArcWake;
use roaring::RoaringBitmap;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::io;
use std::io::IoSlice;
use std::ops::DerefMut;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tracing::debug;

pub struct WarpConn(pub Rc<RefCell<TcpStream>>);

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

pub struct Wk {
    bitmap: Arc<Mutex<RoaringBitmap>>,
    index: u32,
}

impl Wk {
    pub fn new(bitmap: Arc<Mutex<RoaringBitmap>>, index: u32) -> Self {
        Self { bitmap, index }
    }
}

impl ArcWake for Wk {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        debug!("been call wake {}", arc_self.index);
        arc_self.bitmap.lock().unwrap().insert(arc_self.index);
    }
}

pub struct Empty;

impl Display for Empty {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "EMPTY")
    }
}
