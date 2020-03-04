// Copyright 2019 Parity Technologies (UK) Ltd.
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

use crate::protocols_handler::{IntoProtocolsHandler, ProtocolsHandler};
use libp2p_core::{ConnectedPoint, Multiaddr, PeerId, connection::{ConnectionId, ListenerId}};
use std::{error, task::Context, task::Poll};

/// A behaviour for the network. Allows customizing the swarm.
///
/// This trait has been designed to be composable. Multiple implementations can be combined into
/// one that handles all the behaviours at once.
///
/// # Deriving `NetworkBehaviour`
///
/// Crate users can implement this trait by using the the `#[derive(NetworkBehaviour)]`
/// proc macro re-exported by the `libp2p` crate. The macro generates a delegating `trait`
/// implementation for the `struct`, which delegates method calls to all trait members. Any events
/// generated by struct members are delegated to [`NetworkBehaviourEventProcess`] implementations
/// which are expected to be provided by the user.
///
/// Optionally one can implement a custom `poll` function, which needs to be tagged with the
/// `#[behaviour(poll_method = "poll")]` attribute, and would be called last with no parameters.
///
/// By default the derive sets the `NetworkBehaviour::OutEvent` as `()` but this can be overriden
/// with `#[behaviour(out_event = "AnotherType")]`.
///
/// `#[behaviour(ignore)]` can be added on a struct field to disable generation of delegation to
/// the fields which do not implement `NetworkBehaviour`.
pub trait NetworkBehaviour: Send + 'static {
    /// Handler for all the protocols the network behaviour supports.
    type ProtocolsHandler: IntoProtocolsHandler;

    /// Event generated by the `NetworkBehaviour` and that the swarm will report back.
    type OutEvent: Send + 'static;

    /// Creates a new `ProtocolsHandler` for a connection with a peer.
    ///
    /// Every time an incoming connection is opened, and every time we start dialing a node, this
    /// method is called.
    ///
    /// The returned object is a handler for that specific connection, and will be moved to a
    /// background task dedicated to that connection.
    ///
    /// The network behaviour (ie. the implementation of this trait) and the handlers it has
    /// spawned (ie. the objects returned by `new_handler`) can communicate by passing messages.
    /// Messages sent from the handler to the behaviour are injected with `inject_event`, and
    /// the behaviour can send a message to the handler by making `poll` return `SendEvent`.
    fn new_handler(&mut self) -> Self::ProtocolsHandler;

    /// Addresses that this behaviour is aware of for this specific peer, and that may allow
    /// reaching the peer.
    ///
    /// The addresses will be tried in the order returned by this function, which means that they
    /// should be ordered by decreasing likelihood of reachability. In other words, the first
    /// address should be the most likely to be reachable.
    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr>;

    /// Indicates the behaviour that we connected to the node with the given peer id through the
    /// given endpoint.
    ///
    /// This node now has a handler (as spawned by `new_handler`) running in the background.
    fn inject_connected(&mut self, peer_id: PeerId, endpoint: ConnectedPoint);

    /// Indicates the behaviour that we disconnected from the node with the given peer id. The
    /// endpoint is the one we used to be connected to.
    ///
    /// There is no handler running anymore for this node. Any event that has been sent to it may
    /// or may not have been processed by the handler.
    fn inject_disconnected(&mut self, peer_id: &PeerId, endpoint: ConnectedPoint);

    /// Informs the behaviour about an event generated by the handler dedicated to the peer identified by `peer_id`.
    /// for the behaviour.
    ///
    /// The `peer_id` is guaranteed to be in a connected state. In other words, `inject_connected`
    /// has previously been called with this `PeerId`.
    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent
    );

    /// Indicates to the behaviour that we tried to reach an address, but failed.
    ///
    /// If we were trying to reach a specific node, its ID is passed as parameter. If this is the
    /// last address to attempt for the given node, then `inject_dial_failure` is called afterwards.
    fn inject_addr_reach_failure(&mut self, _peer_id: Option<&PeerId>, _addr: &Multiaddr, _error: &dyn error::Error) {
    }

    /// Indicates to the behaviour that we tried to dial all the addresses known for a node, but
    /// failed.
    ///
    /// The `peer_id` is guaranteed to be in a disconnected state. In other words,
    /// `inject_connected` has not been called, or `inject_disconnected` has been called since then.
    fn inject_dial_failure(&mut self, _peer_id: &PeerId) {
    }

    /// Indicates to the behaviour that we have started listening on a new multiaddr.
    fn inject_new_listen_addr(&mut self, _addr: &Multiaddr) {
    }

    /// Indicates to the behaviour that a new multiaddr we were listening on has expired,
    /// which means that we are no longer listening in it.
    fn inject_expired_listen_addr(&mut self, _addr: &Multiaddr) {
    }

    /// Indicates to the behaviour that we have discovered a new external address for us.
    fn inject_new_external_addr(&mut self, _addr: &Multiaddr) {
    }

    /// A listener experienced an error.
    fn inject_listener_error(&mut self, _id: ListenerId, _err: &(dyn std::error::Error + 'static)) {
    }

    /// A listener closed.
    fn inject_listener_closed(&mut self, _id: ListenerId) {
    }

    /// Polls for things that swarm should do.
    ///
    /// This API mimics the API of the `Stream` trait. The method may register the current task in
    /// order to wake it up at a later point in time.
    fn poll(&mut self, cx: &mut Context, params: &mut impl PollParameters)
        -> Poll<NetworkBehaviourAction<<<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::InEvent, Self::OutEvent>>;
}

/// Parameters passed to `poll()`, that the `NetworkBehaviour` has access to.
pub trait PollParameters {
    /// Iterator returned by [`supported_protocols`](PollParameters::supported_protocols).
    type SupportedProtocolsIter: ExactSizeIterator<Item = Vec<u8>>;
    /// Iterator returned by [`listened_addresses`](PollParameters::listened_addresses).
    type ListenedAddressesIter: ExactSizeIterator<Item = Multiaddr>;
    /// Iterator returned by [`external_addresses`](PollParameters::external_addresses).
    type ExternalAddressesIter: ExactSizeIterator<Item = Multiaddr>;

    /// Returns the list of protocol the behaviour supports when a remote negotiates a protocol on
    /// an inbound substream.
    ///
    /// The iterator's elements are the ASCII names as reported on the wire.
    ///
    /// Note that the list is computed once at initialization and never refreshed.
    fn supported_protocols(&self) -> Self::SupportedProtocolsIter;

    /// Returns the list of the addresses we're listening on.
    fn listened_addresses(&self) -> Self::ListenedAddressesIter;

    /// Returns the list of the addresses nodes can use to reach us.
    fn external_addresses(&self) -> Self::ExternalAddressesIter;

    /// Returns the peer id of the local node.
    fn local_peer_id(&self) -> &PeerId;
}

/// When deriving [`NetworkBehaviour`] this trait must be implemented for all the possible event types
/// generated by the inner behaviours.
pub trait NetworkBehaviourEventProcess<TEvent> {
    /// Called when one of the fields of the type you're deriving `NetworkBehaviour` on generates
    /// an event.
    fn inject_event(&mut self, event: TEvent);
}

/// An action that a [`NetworkBehaviour`] can trigger in the [`Swarm`]
/// in whose context it is executing.
///
/// [`Swarm`]: super::Swarm
#[derive(Debug, Clone)]
pub enum NetworkBehaviourAction<TInEvent, TOutEvent> {
    /// Instructs the `Swarm` to return an event when it is being polled.
    GenerateEvent(TOutEvent),

    /// Instructs the swarm to dial the given multiaddress, with no knowledge of the `PeerId` that
    /// may be reached.
    DialAddress {
        /// The address to dial.
        address: Multiaddr,
    },

    /// Instructs the swarm to dial a known `PeerId`.
    ///
    /// The `addresses_of_peer` method is called to determine which addresses to attempt to reach.
    ///
    /// If we were already trying to dial this node, the addresses that are not yet in the queue of
    /// addresses to try are added back to this queue.
    ///
    /// On success, [`NetworkBehaviour::inject_connected`] is invoked.
    /// On failure, [`NetworkBehaviour::inject_dial_failure`] is invoked.
    DialPeer {
        /// The peer to try reach.
        peer_id: PeerId,
    },

    /// Instructs the `Swarm` to send an event to the handler dedicated to a
    /// connection with a peer.
    ///
    /// If the `Swarm` is connected to the peer, the message is delivered to the
    /// `ProtocolsHandler` instance identified by the peer ID and connection ID.
    ///
    /// If the specified connection no longer exists, the event is silently dropped.
    ///
    /// Typically the connection ID given is the same as the one passed to
    /// [`NetworkBehaviour::inject_event`], i.e. whenever the behaviour wishes to
    /// respond to a request on the same connection (and possibly the same
    /// substream, as per the implementation of `ProtocolsHandler`).
    ///
    /// Note that even if the peer is currently connected, connections can get closed
    /// at any time and thus the event may not reach a handler.
    NotifyHandler {
        /// The peer for whom a `ProtocolsHandler` should be notified.
        peer_id: PeerId,
        /// The ID of the connection whose `ProtocolsHandler` to notify.
        handler: NotifyHandler,
        /// The event to send.
        event: TInEvent,
    },

    /// Informs the `Swarm` about a multi-address observed by a remote for
    /// the local node.
    ///
    /// It is advisable to issue `ReportObservedAddr` actions at a fixed frequency
    /// per node. This way address information will be more accurate over time
    /// and individual outliers carry less weight.
    ReportObservedAddr {
        /// The observed address of the local node.
        address: Multiaddr,
    },
}

/// The options w.r.t. which connection handlers to notify of an event.
#[derive(Debug, Clone)]
pub enum NotifyHandler {
    /// Notify a particular connection handler.
    One(ConnectionId),
    /// Notify an arbitrary connection handler.
    Any,
    /// Notify all connection handlers.
    All
}

