// Copyright 2018 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Implementation of the [Yamux](https://github.com/hashicorp/yamux/blob/master/spec.md)  multiplexing protocol for libp2p.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use futures::{future, prelude::*, ready, stream::BoxStream};
use libp2p_core::muxing::{StreamMuxer, StreamMuxerEvent};
use libp2p_core::upgrade::{InboundUpgrade, OutboundUpgrade, UpgradeInfo};
use std::collections::VecDeque;
use std::io::{IoSlice, IoSliceMut};
use std::task::Waker;
use std::{
    fmt, io, iter, mem,
    pin::Pin,
    task::{Context, Poll},
};
use thiserror::Error;
use yamux::ConnectionError;

/// A Yamux connection.
pub struct Muxer<C> {
    /// The [`futures::stream::Stream`] of incoming substreams.
    incoming: BoxStream<'static, Result<yamux::Stream, yamux::ConnectionError>>,
    /// Handle to control the connection.
    control: yamux::Control,
    /// Temporarily buffers inbound streams in case our node is performing backpressure on the remote.
    ///
    /// The only way how yamux can make progress is by driving the stream. However, the
    /// [`StreamMuxer`] interface is designed to allow a caller to selectively make progress via
    /// [`StreamMuxer::poll_inbound`] and [`StreamMuxer::poll_outbound`] whilst the more general
    /// [`StreamMuxer::poll`] is designed to make progress on existing streams etc.
    ///
    /// This buffer stores inbound streams that are created whilst [`StreamMuxer::poll`] is called.
    /// Once the buffer is full, new inbound streams are dropped.
    inbound_stream_buffer: VecDeque<Stream>,
    /// Waker to be called when new inbound streams are available.
    inbound_stream_waker: Option<Waker>,

    _phantom: std::marker::PhantomData<C>,
}

const MAX_BUFFERED_INBOUND_STREAMS: usize = 25;

impl<S> fmt::Debug for Muxer<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Yamux")
    }
}

impl<C> Muxer<C>
where
    C: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Create a new Yamux connection.
    fn new(io: C, cfg: yamux::Config, mode: yamux::Mode) -> Self {
        let conn = yamux::Connection::new(io, cfg, mode);
        let ctrl = conn.control();

        Self {
            incoming: yamux::into_stream(conn).err_into().boxed(),
            control: ctrl,
            inbound_stream_buffer: VecDeque::default(),
            inbound_stream_waker: None,
            _phantom: Default::default(),
        }
    }
}

impl<C> StreamMuxer for Muxer<C>
where
    C: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Substream = Stream;
    type Error = Error;

    fn poll_inbound(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        if let Some(stream) = self.inbound_stream_buffer.pop_front() {
            return Poll::Ready(Ok(stream));
        }

        if let Poll::Ready(res) = self.poll_inner(cx) {
            return Poll::Ready(res);
        }

        self.inbound_stream_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    fn poll_outbound(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        Pin::new(&mut self.control)
            .poll_open_stream(cx)
            .map_ok(Stream)
            .map_err(Error)
    }

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<StreamMuxerEvent, Self::Error>> {
        let this = self.get_mut();

        let inbound_stream = ready!(this.poll_inner(cx))?;

        if this.inbound_stream_buffer.len() >= MAX_BUFFERED_INBOUND_STREAMS {
            log::warn!("dropping {} because buffer is full", inbound_stream.0);
            drop(inbound_stream);
        } else {
            this.inbound_stream_buffer.push_back(inbound_stream);

            if let Some(waker) = this.inbound_stream_waker.take() {
                waker.wake()
            }
        }

        // Schedule an immediate wake-up, allowing other code to run.
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn poll_close(mut self: Pin<&mut Self>, c: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Poll::Ready(()) = Pin::new(&mut self.control).poll_close(c).map_err(Error)? {
            return Poll::Ready(Ok(()));
        }

        while let Poll::Ready(maybe_inbound_stream) =
            self.incoming.poll_next_unpin(c).map_err(Error)?
        {
            match maybe_inbound_stream {
                Some(inbound_stream) => mem::drop(inbound_stream),
                None => return Poll::Ready(Ok(())),
            }
        }

        Poll::Pending
    }
}

/// A stream produced by the yamux multiplexer.
#[derive(Debug)]
pub struct Stream(yamux::Stream);

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }

    fn poll_read_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_read_vectored(cx, bufs)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

impl<C> Muxer<C>
where
    C: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Result<Stream, Error>> {
        self.incoming.poll_next_unpin(cx).map(|maybe_stream| {
            let stream = maybe_stream
                .transpose()
                .map_err(Error)?
                .map(Stream)
                .ok_or(Error(ConnectionError::Closed))?;

            Ok(stream)
        })
    }
}

/// The yamux configuration.
#[derive(Debug, Clone)]
pub struct Config {
    inner: yamux::Config,
    mode: Option<yamux::Mode>,
}

/// The window update mode determines when window updates are
/// sent to the remote, giving it new credit to send more data.
pub struct WindowUpdateMode(yamux::WindowUpdateMode);

impl WindowUpdateMode {
    /// The window update mode whereby the remote is given
    /// new credit via a window update whenever the current
    /// receive window is exhausted when data is received,
    /// i.e. this mode cannot exert back-pressure from application
    /// code that is slow to read from a substream.
    ///
    /// > **Note**: The receive buffer may overflow with this
    /// > strategy if the receiver is too slow in reading the
    /// > data from the buffer. The maximum receive buffer
    /// > size must be tuned appropriately for the desired
    /// > throughput and level of tolerance for (temporarily)
    /// > slow receivers.
    pub fn on_receive() -> Self {
        WindowUpdateMode(yamux::WindowUpdateMode::OnReceive)
    }

    /// The window update mode whereby the remote is given new
    /// credit only when the current receive window is exhausted
    /// when data is read from the substream's receive buffer,
    /// i.e. application code that is slow to read from a substream
    /// exerts back-pressure on the remote.
    ///
    /// > **Note**: If the receive window of a substream on
    /// > both peers is exhausted and both peers are blocked on
    /// > sending data before reading from the stream, a deadlock
    /// > occurs. To avoid this situation, reading from a substream
    /// > should never be blocked on writing to the same substream.
    ///
    /// > **Note**: With this strategy, there is usually no point in the
    /// > receive buffer being larger than the window size.
    pub fn on_read() -> Self {
        WindowUpdateMode(yamux::WindowUpdateMode::OnRead)
    }
}

impl Config {
    /// Creates a new `YamuxConfig` in client mode, regardless of whether
    /// it will be used for an inbound or outbound upgrade.
    pub fn client() -> Self {
        Self {
            mode: Some(yamux::Mode::Client),
            ..Default::default()
        }
    }

    /// Creates a new `YamuxConfig` in server mode, regardless of whether
    /// it will be used for an inbound or outbound upgrade.
    pub fn server() -> Self {
        Self {
            mode: Some(yamux::Mode::Server),
            ..Default::default()
        }
    }

    /// Sets the size (in bytes) of the receive window per substream.
    pub fn set_receive_window_size(&mut self, num_bytes: u32) -> &mut Self {
        self.inner.set_receive_window(num_bytes);
        self
    }

    /// Sets the maximum size (in bytes) of the receive buffer per substream.
    pub fn set_max_buffer_size(&mut self, num_bytes: usize) -> &mut Self {
        self.inner.set_max_buffer_size(num_bytes);
        self
    }

    /// Sets the maximum number of concurrent substreams.
    pub fn set_max_num_streams(&mut self, num_streams: usize) -> &mut Self {
        self.inner.set_max_num_streams(num_streams);
        self
    }

    /// Sets the window update mode that determines when the remote
    /// is given new credit for sending more data.
    pub fn set_window_update_mode(&mut self, mode: WindowUpdateMode) -> &mut Self {
        self.inner.set_window_update_mode(mode.0);
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut inner = yamux::Config::default();
        // For conformity with mplex, read-after-close on a multiplexed
        // connection is never permitted and not configurable.
        inner.set_read_after_close(false);
        Config { inner, mode: None }
    }
}

impl UpgradeInfo for Config {
    type Info = &'static str;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once("/yamux/1.0.0")
    }
}

impl<C> InboundUpgrade<C> for Config
where
    C: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = Muxer<C>;
    type Error = io::Error;
    type Future = future::Ready<Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, io: C, _: Self::Info) -> Self::Future {
        let mode = self.mode.unwrap_or(yamux::Mode::Server);
        future::ready(Ok(Muxer::new(io, self.inner, mode)))
    }
}

impl<C> OutboundUpgrade<C> for Config
where
    C: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = Muxer<C>;
    type Error = io::Error;
    type Future = future::Ready<Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, io: C, _: Self::Info) -> Self::Future {
        let mode = self.mode.unwrap_or(yamux::Mode::Client);
        future::ready(Ok(Muxer::new(io, self.inner, mode)))
    }
}

/// The Yamux [`StreamMuxer`] error type.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct Error(yamux::ConnectionError);

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        match err.0 {
            yamux::ConnectionError::Io(e) => e,
            e => io::Error::new(io::ErrorKind::Other, e),
        }
    }
}
