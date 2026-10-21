//! A body that forwards every frame as it arrives and keeps a copy of the
//! data (shared `Bytes`, no copying) for the journal, up to a limit. When
//! the body ends, breaks off, or is dropped before its end (the reader went
//! away), the copy is handed to a callback, once.
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

/// How a body ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Complete,
    /// The sender's stream broke off with this error.
    Error(String),
    /// Dropped before its end (the reader went away).
    Dropped,
}

/// The data of a body as it passed through.
#[derive(Clone, Debug)]
pub struct Captured {
    pub chunks: Vec<Bytes>,
    pub bytes: usize,
    /// More data passed than was kept.
    pub truncated: bool,
    pub end: End,
}

impl Captured {
    pub fn concat(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.bytes);
        for c in &self.chunks {
            v.extend_from_slice(c);
        }
        v
    }
}

type Done = Box<dyn FnOnce(Captured) + Send>;

pub struct Tee<B: Body<Data = Bytes> + Unpin> {
    inner: B,
    chunks: Vec<Bytes>,
    bytes: usize,
    truncated: bool,
    max: usize,
    done: Option<Done>,
    /// Kept alive as long as the body (e.g. an in-flight counter guard).
    _guard: Option<Box<dyn Send>>,
}

impl<B: Body<Data = Bytes> + Unpin> Tee<B> {
    /// Copy up to `max` bytes and call `done` at the end.
    pub fn new(inner: B, max: usize, done: impl FnOnce(Captured) + Send + 'static) -> Self {
        Self { inner, chunks: Vec::new(), bytes: 0, truncated: false, max, done: Some(Box::new(done)), _guard: None }
    }

    /// Forward only (nothing is kept), holding `guard` while the body lives.
    pub fn plain(inner: B) -> Self {
        Self { inner, chunks: Vec::new(), bytes: 0, truncated: false, max: 0, done: None, _guard: None }
    }

    pub fn with_guard(mut self, guard: impl Send + 'static) -> Self {
        self._guard = Some(Box::new(guard));
        self
    }

    fn keep(&mut self, d: &Bytes) {
        if self.done.is_none() || self.truncated {
            return;
        }
        if self.bytes + d.len() > self.max {
            self.truncated = true;
            self.chunks = Vec::new(); // a cut body cannot be parsed: free it now
            return;
        }
        self.bytes += d.len();
        self.chunks.push(d.clone());
    }

    fn finish(&mut self, end: End) {
        if let Some(done) = self.done.take() {
            done(Captured {
                chunks: std::mem::take(&mut self.chunks),
                bytes: self.bytes,
                truncated: self.truncated,
                end,
            });
        }
    }
}

impl<B> Body for Tee<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(f))) => {
                if let Some(d) = f.data_ref() {
                    this.keep(d);
                }
                if this.inner.is_end_stream() {
                    this.finish(End::Complete);
                }
                Poll::Ready(Some(Ok(f)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.finish(End::Error(e.to_string()));
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                this.finish(End::Complete);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B: Body<Data = Bytes> + Unpin> Drop for Tee<B> {
    fn drop(&mut self) {
        let end = if self.inner.is_end_stream() { End::Complete } else { End::Dropped };
        self.finish(end);
    }
}
