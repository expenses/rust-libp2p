// Copyright 2017-2020 Parity Technologies (UK) Ltd.
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
use super::{
    certificate,
    endpoint::{EndpointData, EndpointInner},
    error::Error,
    verifier,
};
use async_macros::ready;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use libp2p_core::StreamMuxer;
use log::{debug, error, trace, warn};
use parking_lot::{Mutex, MutexGuard};
use quinn_proto::{Connection, ConnectionEvent, ConnectionHandle, Dir, StreamId};
use std::{
    collections::HashMap,
    io,
    mem::replace,
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll},
    time::Instant,
};
#[derive(Debug)]
pub struct QuicSubstream {
    id: StreamId,
    status: SubstreamStatus,
}

#[derive(Debug)]
enum SubstreamStatus {
    Live,
    Finishing(oneshot::Receiver<()>),
    Finished,
}

impl QuicSubstream {
    fn new(id: StreamId) -> Self {
        let status = SubstreamStatus::Live;
        Self { id, status }
    }

    fn is_live(&self) -> bool {
        match self.status {
            SubstreamStatus::Live => true,
            SubstreamStatus::Finishing(_) | SubstreamStatus::Finished => false,
        }
    }
}

/// Represents the configuration for a QUIC/UDP/IP transport capability for libp2p.
///
/// The QUIC endpoints created by libp2p will need to be progressed by running the futures and streams
/// obtained by libp2p through the tokio reactor.
#[derive(Debug, Clone)]
pub struct QuicConfig {
    /// The client configuration.  Quinn provides functions for making one.
    pub client_config: quinn_proto::ClientConfig,
    /// The server configuration.  Quinn provides functions for making one.
    pub server_config: Arc<quinn_proto::ServerConfig>,
    /// The endpoint configuration
    pub endpoint_config: Arc<quinn_proto::EndpointConfig>,
}

fn make_client_config(
    certificate: rustls::Certificate,
    key: rustls::PrivateKey,
) -> quinn_proto::ClientConfig {
    let mut transport = quinn_proto::TransportConfig::default();
    transport.stream_window_uni(0);
    transport.datagram_receive_buffer_size(None);
    use std::time::Duration;
    transport.keep_alive_interval(Some(Duration::from_millis(1000)));
    let mut crypto = rustls::ClientConfig::new();
    crypto.versions = vec![rustls::ProtocolVersion::TLSv1_3];
    crypto.enable_early_data = true;
    crypto.set_single_client_cert(vec![certificate], key);
    let verifier = verifier::VeryInsecureRequireExactlyOneServerCertificateButDoNotCheckIt;
    crypto
        .dangerous()
        .set_certificate_verifier(Arc::new(verifier));
    quinn_proto::ClientConfig {
        transport: Arc::new(transport),
        crypto: Arc::new(crypto),
    }
}

fn make_server_config(
    certificate: rustls::Certificate,
    key: rustls::PrivateKey,
) -> quinn_proto::ServerConfig {
    let mut transport = quinn_proto::TransportConfig::default();
    transport.stream_window_uni(0);
    transport.datagram_receive_buffer_size(None);
    let mut crypto = rustls::ServerConfig::new(Arc::new(
        verifier::VeryInsecureRequireExactlyOneClientCertificateButDoNotCheckIt,
    ));
    crypto.versions = vec![rustls::ProtocolVersion::TLSv1_3];
    crypto
        .set_single_cert(vec![certificate], key)
        .expect("we are given a valid cert; qed");
    let mut config = quinn_proto::ServerConfig::default();
    config.transport = Arc::new(transport);
    config.crypto = Arc::new(crypto);
    config
}

impl QuicConfig {
    /// Creates a new configuration object for TCP/IP.
    pub fn new(keypair: &libp2p_core::identity::Keypair) -> Self {
        let cert = super::make_cert(&keypair);
        let (cert, key) = (
            rustls::Certificate(
                cert.serialize_der()
                    .expect("serialization of a valid cert will succeed; qed"),
            ),
            rustls::PrivateKey(cert.serialize_private_key_der()),
        );
        Self {
            client_config: make_client_config(cert.clone(), key.clone()),
            server_config: Arc::new(make_server_config(cert, key)),
            endpoint_config: Default::default(),
        }
    }
}

#[derive(Debug)]
pub(super) enum EndpointMessage {
    ConnectionAccepted,
    EndpointEvent {
        handle: ConnectionHandle,
        event: quinn_proto::EndpointEvent,
    },
}

#[derive(Debug, Clone)]
pub struct QuicMuxer(Arc<Mutex<Muxer>>);

impl QuicMuxer {
    fn inner<'a>(&'a self) -> MutexGuard<'a, Muxer> {
        self.0.lock()
    }
}

#[derive(Debug)]
enum OutboundInner {
    Complete(Result<StreamId, Error>),
    Pending(oneshot::Receiver<StreamId>),
    Done,
}

pub struct Outbound(OutboundInner);

impl Future for Outbound {
    type Output = Result<QuicSubstream, Error>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = &mut *self;
        match this.0 {
            OutboundInner::Complete(_) => match replace(&mut this.0, OutboundInner::Done) {
                OutboundInner::Complete(e) => Poll::Ready(e.map(QuicSubstream::new)),
                _ => unreachable!(),
            },
            OutboundInner::Pending(ref mut receiver) => {
                let result = ready!(receiver.poll_unpin(cx))
                    .map(QuicSubstream::new)
                    .map_err(|oneshot::Canceled| Error::ConnectionLost);
                this.0 = OutboundInner::Done;
                Poll::Ready(result)
            }
            OutboundInner::Done => panic!("polled after yielding Ready"),
        }
    }
}

impl StreamMuxer for QuicMuxer {
    type OutboundSubstream = Outbound;
    type Substream = QuicSubstream;
    type Error = crate::error::Error;
    fn open_outbound(&self) -> Self::OutboundSubstream {
        let mut inner = self.inner();
        if let Some(ref e) = inner.close_reason {
            Outbound(OutboundInner::Complete(Err(Error::ConnectionError(
                e.clone(),
            ))))
        } else if let Some(id) = inner.get_pending_stream() {
            Outbound(OutboundInner::Complete(Ok(id)))
        } else {
            let (sender, receiver) = oneshot::channel();
            inner.connectors.push_front(sender);
            inner.wake_driver();
            Outbound(OutboundInner::Pending(receiver))
        }
    }
    fn destroy_outbound(&self, _: Outbound) {}
    fn destroy_substream(&self, substream: Self::Substream) {
        let mut inner = self.inner();
        if let Some(waker) = inner.writers.remove(&substream.id) {
            waker.wake();
        }
        if let Some(waker) = inner.readers.remove(&substream.id) {
            waker.wake();
        }
        if substream.is_live() && inner.close_reason.is_none() {
            if let Err(e) = inner.connection.finish(substream.id) {
                warn!("Error closing stream: {}", e);
            }
        }
        drop(
            inner
                .connection
                .stop_sending(substream.id, Default::default()),
        )
    }
    fn is_remote_acknowledged(&self) -> bool {
        true
    }

    fn poll_inbound(&self, cx: &mut Context) -> Poll<Result<Self::Substream, Self::Error>> {
        debug!("being polled for inbound connections!");
        let mut inner = self.inner();
        if inner.connection.is_drained() {
            return Poll::Ready(Err(Error::ConnectionError(
                inner
                    .close_reason
                    .clone()
                    .expect("closed connections always have a reason; qed"),
            )));
        }
        inner.wake_driver();
        match inner.connection.accept(quinn_proto::Dir::Bi) {
            None => {
                if let Some(waker) = replace(&mut inner.accept_waker, Some(cx.waker().clone())) {
                    waker.wake()
                }
                Poll::Pending
            }
            Some(id) => {
                inner.finishers.insert(id, None);
                Poll::Ready(Ok(QuicSubstream::new(id)))
            }
        }
    }

    fn write_substream(
        &self,
        cx: &mut Context,
        substream: &mut Self::Substream,
        buf: &[u8],
    ) -> Poll<Result<usize, Self::Error>> {
        use quinn_proto::WriteError;
        if !substream.is_live() {
            error!(
                "The application used stream {:?} after it was no longer live",
                substream.id
            );
            return Poll::Ready(Err(Error::ExpiredStream));
        }
        let mut inner = self.inner();
        debug_assert!(
            inner.finishers.get(&substream.id).is_some(),
            "no entry in finishers map for write stream"
        );
        inner.wake_driver();
        if let Some(ref e) = inner.close_reason {
            return Poll::Ready(Err(Error::ConnectionError(e.clone())));
        }
        assert!(
            !inner.connection.is_drained(),
            "attempting to write to a drained connection"
        );
        match inner.connection.write(substream.id, buf) {
            Ok(bytes) => Poll::Ready(Ok(bytes)),
            Err(WriteError::Blocked) => {
                if let Some(ref e) = inner.close_reason {
                    return Poll::Ready(Err(Error::ConnectionError(e.clone())));
                }
                if let Some(w) = inner.writers.insert(substream.id, cx.waker().clone()) {
                    w.wake();
                }
                Poll::Pending
            }
            Err(WriteError::UnknownStream) => {
                error!(
                    "The application used a stream that has already been closed. This is a bug."
                );
                Poll::Ready(Err(Error::ExpiredStream))
            }
            Err(WriteError::Stopped(e)) => {
                inner.finishers.remove(&substream.id);
                if let Some(w) = inner.writers.remove(&substream.id) {
                    w.wake()
                }
                substream.status = SubstreamStatus::Finished;
                Poll::Ready(Err(Error::Stopped(e)))
            }
        }
    }

    fn poll_outbound(
        &self,
        cx: &mut Context,
        substream: &mut Self::OutboundSubstream,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        substream.poll_unpin(cx)
    }

    fn read_substream(
        &self,
        cx: &mut Context,
        substream: &mut Self::Substream,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Self::Error>> {
        use quinn_proto::ReadError;
        let mut inner = self.inner();
        inner.wake_driver();
        match inner.connection.read(substream.id, buf) {
            Ok(Some(bytes)) => Poll::Ready(Ok(bytes)),
            Ok(None) => Poll::Ready(Ok(0)),
            Err(ReadError::Blocked) => match &inner.close_reason {
                None => {
                    trace!(
                        "Blocked on reading stream {:?} with side {:?}",
                        substream.id,
                        inner.connection.side()
                    );
                    if let Some(w) = inner.readers.insert(substream.id, cx.waker().clone()) {
                        w.wake()
                    }
                    Poll::Pending
                }
                // KLUDGE: this is a workaround for https://github.com/djc/quinn/issues/604.
                Some(quinn_proto::ConnectionError::ApplicationClosed(
                    quinn_proto::ApplicationClose { error_code, reason },
                )) if error_code.into_inner() == 0 && reason.is_empty() => {
                    warn!("This should not happen, but a quinn-proto bug causes it to happen");
                    if let Some(w) = inner.readers.remove(&substream.id) {
                        w.wake()
                    }
                    Poll::Ready(Ok(0))
                }
                Some(error) => Poll::Ready(Err(Error::ConnectionError(error.clone()))),
            },
            Err(ReadError::UnknownStream) => {
                error!(
                    "The application used a stream that has already been closed. This is a bug."
                );
                Poll::Ready(Err(Error::ExpiredStream))
            }
            Err(ReadError::Reset(e)) => {
                if let Some(w) = inner.readers.remove(&substream.id) {
                    w.wake()
                }
                Poll::Ready(Err(Error::Reset(e)))
            }
        }
    }

    fn shutdown_substream(
        &self,
        cx: &mut Context,
        substream: &mut Self::Substream,
    ) -> Poll<Result<(), Self::Error>> {
        match substream.status {
            SubstreamStatus::Finished => return Poll::Ready(Ok(())),
            SubstreamStatus::Finishing(ref mut channel) => {
                self.inner().wake_driver();
                return channel.poll_unpin(cx).map_err(|e| {
                    Error::IO(io::Error::new(io::ErrorKind::ConnectionAborted, e.clone()))
                });
            }
            SubstreamStatus::Live => {}
        }
        let mut inner = self.inner();
        inner.wake_driver();
        inner.connection.finish(substream.id).map_err(|e| match e {
            quinn_proto::FinishError::UnknownStream => unreachable!("we checked for this above!"),
            quinn_proto::FinishError::Stopped(e) => Error::Stopped(e),
        })?;
        let (sender, mut receiver) = oneshot::channel();
        assert!(
            receiver.poll_unpin(cx).is_pending(),
            "we haven’t written to the peer yet"
        );
        substream.status = SubstreamStatus::Finishing(receiver);
        match inner.finishers.insert(substream.id, Some(sender)) {
            Some(None) => {}
            _ => unreachable!(
                "We inserted a None value earlier; and haven’t touched this entry since; qed"
            ),
        }
        Poll::Pending
    }

    fn flush_substream(
        &self,
        _cx: &mut Context,
        _substream: &mut Self::Substream,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn flush_all(&self, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn close(&self, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        trace!("close() called");
        let mut inner = self.inner();
        if inner.connection.is_closed() {
            return Poll::Ready(Ok(()));
        } else if inner.close_reason.is_some() {
            return Poll::Ready(Ok(()));
        } else if inner.finishers.is_empty() {
            inner.shutdown(0);
            inner.close_reason = Some(quinn_proto::ConnectionError::LocallyClosed);
            drop(inner.driver().poll_unpin(cx));
            return Poll::Ready(Ok(()));
        } else if inner.close_waker.is_some() {
            inner.close_waker = Some(cx.waker().clone());
            return Poll::Pending;
        } else {
            inner.close_waker = Some(cx.waker().clone())
        }
        let Muxer {
            finishers,
            connection,
            ..
        } = &mut *inner;
        for (id, channel) in finishers {
            if channel.is_none() {
                match connection.finish(*id) {
                    Ok(()) => {}
                    Err(error) => warn!("Finishing stream {:?} failed: {}", id, error),
                }
            }
        }
        Poll::Pending
    }
}

#[derive(Debug)]
pub struct QuicUpgrade {
    muxer: Option<QuicMuxer>,
}

#[cfg(test)]
impl Drop for QuicUpgrade {
    fn drop(&mut self) {
        debug!("dropping upgrade!");
        assert!(
            self.muxer.is_none(),
            "dropped before being polled to completion"
        );
    }
}

impl Future for QuicUpgrade {
    type Output = Result<(libp2p_core::PeerId, QuicMuxer), Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let muxer = &mut self.get_mut().muxer;
        trace!("outbound polling!");
        let res = {
            let mut inner = muxer.as_mut().expect("polled after yielding Ready").inner();
            if inner.connection.is_closed() {
                return Poll::Ready(Err(Error::ConnectionError(
                    inner
                        .close_reason
                        .clone()
                        .expect("closed connections always have a reason; qed"),
                )));
            } else if inner.connection.is_handshaking() {
                assert!(!inner.connection.is_closed(), "deadlock");
                inner.handshake_waker = Some(cx.waker().clone());
                return Poll::Pending;
            } else if inner.connection.side().is_server() {
                ready!(inner.endpoint_channel.poll_ready(cx))
                    .expect("we have a reference to the peer; qed");
                inner
                    .endpoint_channel
                    .start_send(EndpointMessage::ConnectionAccepted)
                    .expect("we just checked that we have capacity to send this; qed")
            }

            let peer_certificates: Vec<rustls::Certificate> = inner
                .connection
                .crypto_session()
                .get_peer_certificates()
                .expect("we have finished handshaking, so we have exactly one certificate; qed");
            certificate::verify_libp2p_certificate(
                peer_certificates
                    .get(0)
                    .expect(
                        "our certificate verifiers require exactly one \
                         certificate to be presented, so an empty certificate \
                         chain would already have been rejected; qed",
                    )
                    .as_ref(),
            )
        };
        let muxer = muxer.take().expect("polled after yielding Ready");
        Poll::Ready(match res {
            Ok(e) => Ok((e, muxer)),
            Err(ring::error::Unspecified) => Err(Error::BadCertificate(ring::error::Unspecified)),
        })
    }
}

type StreamSenderQueue = std::collections::VecDeque<oneshot::Sender<StreamId>>;

#[derive(Debug)]
pub(crate) struct Muxer {
    /// The pending stream, if any.
    pending_stream: Option<StreamId>,
    /// The associated endpoint
    endpoint: Arc<EndpointData>,
    /// The `quinn_proto::Connection` struct.
    connection: Connection,
    /// Connection handle
    handle: ConnectionHandle,
    /// Tasks blocked on writing
    writers: HashMap<StreamId, std::task::Waker>,
    /// Tasks blocked on reading
    readers: HashMap<StreamId, std::task::Waker>,
    /// Tasks blocked on finishing
    finishers: HashMap<StreamId, Option<oneshot::Sender<()>>>,
    /// Task waiting for new connections
    handshake_waker: Option<std::task::Waker>,
    /// Task waiting for new connections
    accept_waker: Option<std::task::Waker>,
    /// Tasks waiting to make a connection
    connectors: StreamSenderQueue,
    /// Pending transmit
    pending: Option<quinn_proto::Transmit>,
    /// The timer being used by this connection
    timer: Option<futures_timer::Delay>,
    /// The close reason, if this connection has been lost
    close_reason: Option<quinn_proto::ConnectionError>,
    /// Waker to wake up the driver
    waker: Option<std::task::Waker>,
    /// Channel for endpoint events
    endpoint_channel: mpsc::Sender<EndpointMessage>,
    /// Last timeout
    last_timeout: Option<Instant>,
    /// Join handle for the driver
    driver: Option<async_std::task::JoinHandle<Result<(), Error>>>,
    /// Close waker
    close_waker: Option<std::task::Waker>,
    /// Have we gotten a connection lost event?
    connection_lost: bool,
}

const RESET: u32 = 1;

impl Drop for Muxer {
    fn drop(&mut self) {
        if self.close_reason.is_none() {
            self.shutdown(RESET)
        }
    }
}

impl Drop for QuicMuxer {
    fn drop(&mut self) {
        let inner = self.inner();
        debug!("dropping muxer with side {:?}", inner.connection.side());
        #[cfg(test)]
        assert!(
            !inner.connection.is_handshaking(),
            "dropped a connection that was still handshaking"
        );
    }
}

impl Muxer {
    fn wake_driver(&mut self) {
        if let Some(waker) = self.waker.take() {
            debug!("driver awoken!");
            waker.wake();
        }
    }

    fn driver(&mut self) -> &mut async_std::task::JoinHandle<Result<(), Error>> {
        self.driver
            .as_mut()
            .expect("we don’t call this until the driver is spawned; qed")
    }

    fn drive_timer(&mut self, cx: &mut Context, now: Instant) -> bool {
        let mut keep_going = false;
        loop {
            match self.connection.poll_timeout() {
                None => {
                    self.timer = None;
                    self.last_timeout = None
                }
                Some(t) if t <= now => {
                    self.connection.handle_timeout(now);
                    keep_going = true;
                    continue;
                }
                t if t == self.last_timeout => {}
                t => {
                    let delay = t.expect("already checked to be Some; qed") - now;
                    self.timer = Some(futures_timer::Delay::new(delay))
                }
            }
            if let Some(ref mut timer) = self.timer {
                if timer.poll_unpin(cx).is_ready() {
                    self.connection.handle_timeout(now);
                    keep_going = true;
                    continue;
                }
            }
            break;
        }

        keep_going
    }

    fn new(endpoint: Arc<EndpointData>, connection: Connection, handle: ConnectionHandle) -> Self {
        Muxer {
            connection_lost: false,
            close_waker: None,
            last_timeout: None,
            pending_stream: None,
            connection,
            handle,
            writers: HashMap::new(),
            readers: HashMap::new(),
            finishers: HashMap::new(),
            accept_waker: None,
            handshake_waker: None,
            connectors: Default::default(),
            endpoint_channel: endpoint.event_channel(),
            endpoint: endpoint,
            pending: None,
            timer: None,
            close_reason: None,
            waker: None,
            driver: None,
        }
    }

    /// Process all endpoint-facing events for this connection.  This is synchronous and will not
    /// fail.
    fn send_to_endpoint(&mut self, endpoint: &mut EndpointInner) {
        while let Some(endpoint_event) = self.connection.poll_endpoint_events() {
            if let Some(connection_event) = endpoint.handle_event(self.handle, endpoint_event) {
                self.connection.handle_event(connection_event)
            }
        }
    }

    /// Send endpoint events.  Returns true if and only if there are endpoint events remaining to
    /// be sent.
    fn poll_endpoint_events(&mut self, cx: &mut Context<'_>) -> bool {
        let mut keep_going = false;
        loop {
            match self.endpoint_channel.poll_ready(cx) {
                Poll::Pending => break keep_going,
                Poll::Ready(Err(_)) => unreachable!("we have a reference to the peer; qed"),
                Poll::Ready(Ok(())) => {}
            }
            if let Some(event) = self.connection.poll_endpoint_events() {
                keep_going = true;
                self.endpoint_channel
                    .start_send(EndpointMessage::EndpointEvent {
                        handle: self.handle,
                        event,
                    })
                    .expect("we just checked that we have capacity; qed");
            } else {
                break keep_going;
            }
        }
    }

    fn pre_application_io(&mut self, now: Instant, cx: &mut Context<'_>) -> Result<bool, Error> {
        let mut needs_timer_update = false;
        if let Some(transmit) = self.pending.take() {
            trace!("trying to send packet!");
            if self.poll_transmit(cx, transmit)? {
                return Ok(false);
            }
            trace!("packet sent!");
        }
        while let Some(transmit) = self.connection.poll_transmit(now) {
            trace!("trying to send packet!");
            needs_timer_update = true;
            if self.poll_transmit(cx, transmit)? {
                break;
            }
            trace!("packet sent!");
        }
        Ok(needs_timer_update)
    }

    fn poll_transmit(
        &mut self,
        cx: &mut Context<'_>,
        transmit: quinn_proto::Transmit,
    ) -> Result<bool, Error> {
        let res = self.endpoint.socket().poll_send_to(cx, &transmit);
        match res {
            Poll::Pending => {
                self.pending = Some(transmit);
                Ok(true)
            }
            Poll::Ready(Ok(_)) => Ok(false),
            Poll::Ready(Err(e)) => Err(e.into()),
        }
    }

    fn shutdown(&mut self, error_code: u32) {
        debug!("shutting connection down!");
        if let Some(w) = self.accept_waker.take() {
            w.wake()
        }
        if let Some(w) = self.handshake_waker.take() {
            w.wake()
        }
        for (_, v) in self.writers.drain() {
            v.wake();
        }
        for (_, v) in self.readers.drain() {
            v.wake();
        }
        for sender in self.finishers.drain().filter_map(|x| x.1) {
            drop(sender.send(()))
        }
        self.connectors.truncate(0);
        if !self.connection.is_closed() {
            self.connection.close(
                Instant::now(),
                quinn_proto::VarInt::from_u32(error_code),
                Default::default(),
            );
            self.process_app_events();
        }
        self.wake_driver();
    }

    /// Process application events
    pub(crate) fn process_connection_events(
        &mut self,
        endpoint: &mut EndpointInner,
        event: Option<ConnectionEvent>,
    ) {
        if let Some(event) = event {
            self.connection.handle_event(event);
        }
        if self.connection.is_drained() {
            return;
        }
        self.send_to_endpoint(endpoint);
        self.process_app_events();
        self.wake_driver();
        assert!(self.connection.poll_endpoint_events().is_none());
        assert!(self.connection.poll().is_none());
    }

    fn get_pending_stream(&mut self) -> Option<StreamId> {
        self.wake_driver();
        if let Some(id) = self.pending_stream.take() {
            Some(id)
        } else {
            self.connection.open(Dir::Bi)
        }
        .map(|id| {
            self.finishers.insert(id, None);
            id
        })
    }

    pub fn process_app_events(&mut self) -> bool {
        use quinn_proto::Event;
        let mut keep_going = false;
        'a: while let Some(event) = self.connection.poll() {
            keep_going = true;
            match event {
                Event::StreamOpened { dir: Dir::Uni } | Event::DatagramReceived => {
                    panic!("we disabled incoming unidirectional streams and datagrams")
                }
                Event::StreamAvailable { dir: Dir::Uni } => {
                    panic!("we don’t use unidirectional streams")
                }
                Event::StreamReadable { stream } => {
                    trace!(
                        "Stream {:?} readable for side {:?}",
                        stream,
                        self.connection.side()
                    );
                    // Wake up the task waiting on us (if any)
                    if let Some((_, waker)) = self.readers.remove_entry(&stream) {
                        waker.wake()
                    }
                }
                Event::StreamWritable { stream } => {
                    trace!(
                        "Stream {:?} writable for side {:?}",
                        stream,
                        self.connection.side()
                    );
                    // Wake up the task waiting on us (if any)
                    if let Some((_, waker)) = self.writers.remove_entry(&stream) {
                        waker.wake()
                    }
                }
                Event::StreamAvailable { dir: Dir::Bi } => {
                    trace!(
                        "Bidirectional stream available for side {:?}",
                        self.connection.side()
                    );
                    if self.connectors.is_empty() {
                        // no task to wake up
                        continue;
                    }
                    assert!(
                        self.pending_stream.is_none(),
                        "we cannot have both pending tasks and a pending stream; qed"
                    );
                    let stream = self.connection.open(Dir::Bi)
                            .expect("we just were told that there is a stream available; there is a mutex that prevents other threads from calling open() in the meantime; qed");
                    while let Some(oneshot) = self.connectors.pop_front() {
                        match oneshot.send(stream) {
                            Ok(()) => continue 'a,
                            Err(_) => {}
                        }
                    }
                    self.pending_stream = Some(stream)
                }
                Event::ConnectionLost { reason } => {
                    debug!("lost connection due to {:?}", reason);
                    assert!(self.connection.is_closed());
                    self.close_reason = Some(reason);
                    self.connection_lost = true;
                    self.close_waker.take().map(|e| e.wake());
                    self.shutdown(0);
                }
                Event::StreamFinished {
                    stream,
                    stop_reason,
                } => {
                    // Wake up the task waiting on us (if any)
                    trace!(
                        "Stream {:?} finished for side {:?} because of {:?}",
                        stream,
                        self.connection.side(),
                        stop_reason
                    );
                    if let Some(waker) = self.writers.remove(&stream) {
                        waker.wake()
                    }
                    if let Some(sender) = self.finishers.remove(&stream).expect(
                        "every write stream is placed in this map, and entries are removed \
                         exactly once; qed",
                    ) {
                        drop(sender.send(()))
                    }
                    if self.finishers.is_empty() {
                        self.close_waker.take().map(|e| e.wake());
                    }
                }
                Event::Connected => {
                    debug!("connected!");
                    assert!(!self.connection.is_handshaking(), "quinn-proto bug");
                    if let Some(w) = self.handshake_waker.take() {
                        w.wake()
                    }
                }
                Event::StreamOpened { dir: Dir::Bi } => {
                    debug!("stream opened for side {:?}", self.connection.side());
                    if let Some(w) = self.accept_waker.take() {
                        w.wake()
                    }
                }
            }
        }
        keep_going
    }
}

#[derive(Debug)]
pub(super) struct ConnectionDriver {
    inner: Arc<Mutex<Muxer>>,
    outgoing_packet: Option<quinn_proto::Transmit>,
}

impl ConnectionDriver {
    pub(crate) fn spawn<T: FnOnce(Weak<Mutex<Muxer>>)>(
        endpoint: Arc<EndpointData>,
        connection: Connection,
        handle: ConnectionHandle,
        cb: T,
    ) -> QuicUpgrade {
        let inner = Arc::new(Mutex::new(Muxer::new(endpoint, connection, handle)));
        cb(Arc::downgrade(&inner));
        let handle = async_std::task::spawn(Self {
            inner: inner.clone(),
            outgoing_packet: None,
        });
        inner.lock().driver = Some(handle);
        QuicUpgrade {
            muxer: Some(QuicMuxer(inner)),
        }
    }
}

impl Future for ConnectionDriver {
    type Output = Result<(), Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        // cx.waker().wake_by_ref();
        let this = self.get_mut();
        debug!("being polled for timer!");
        let mut inner = this.inner.lock();
        inner.waker = Some(cx.waker().clone());
        let now = Instant::now();
        loop {
            let mut needs_timer_update = false;
            needs_timer_update |= inner.drive_timer(cx, now);
            needs_timer_update |= inner.pre_application_io(now, cx)?;
            needs_timer_update |= inner.process_app_events();
            needs_timer_update |= inner.poll_endpoint_events(cx);
            if inner.connection.is_drained() {
                break Poll::Ready(
                    match inner
                        .close_reason
                        .clone()
                        .expect("we never have a closed connection with no reason; qed")
                    {
                        quinn_proto::ConnectionError::LocallyClosed => {
                            if needs_timer_update {
                                debug!("continuing until all events are finished");
                                continue;
                            } else {
                                debug!("exiting driver");
                                Ok(())
                            }
                        }
                        e => Err(e.into()),
                    },
                );
            } else if !needs_timer_update {
                break Poll::Pending;
            }
        }
    }
}