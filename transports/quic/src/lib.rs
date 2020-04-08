// Copyright 2020 Parity Technologies (UK) Ltd.
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

//! Implementation of the libp2p `Transport` and `StreamMuxer` traits for QUIC.
//!
//! # Usage
//!
//! Example:
//!
//! ```
//! use libp2p_quic::{Config, Endpoint};
//! use libp2p_core::Multiaddr;
//!
//! let keypair = libp2p_core::identity::Keypair::generate_ed25519();
//! let addr = "/ip4/127.0.0.1/udp/12345/quic".parse().expect("bad address?");
//! let quic_config = Config::new(&keypair, addr).expect("could not make config");
//! let quic_endpoint = Endpoint::new(quic_config).expect("I/O error");
//! ```
//!
//! The `Endpoint` struct implements the `Transport` trait of the `core` library. See the
//! documentation of `core` and of libp2p in general to learn how to use the `Transport` trait.
//!
//! Note that QUIC provides transport, security, and multiplexing in a single protocol.  Therefore,
//! QUIC connections do not need to be upgraded. You will get a compile-time error if you try.
//! Instead, you must pass all needed configuration into the constructor.
//!
//! # Design Notes
//!
//! The entry point is the `Endpoint` struct.  It represents a single QUIC endpoint.  You
//! should generally have one of these per process.
//!
//! `Endpoint` manages a background task that processes all incoming packets.  Each
//! `QuicConnection` also manages a background task, which handles socket output and timer polling.

#![deny(
    const_err,
    deprecated,
    improper_ctypes,
    non_shorthand_field_patterns,
    nonstandard_style,
    no_mangle_generic_items,
    renamed_and_removed_lints,
    unknown_lints,
    type_alias_bounds,
    unconditional_recursion,
    while_true,
    elided_lifetimes_in_paths,
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    single_use_lifetimes,
    trivial_casts,
    trivial_numeric_casts,
    rust_2018_idioms,
    unused,
    future_incompatible,
    clippy::all
)]
#![forbid(unsafe_code, intra_doc_link_resolution_failure)]

#[cfg(not(feature = "tracing"))]
use log::{debug, error, info, trace, warn};
#[cfg(feature = "tracing")]
use tracing::{debug, error, info, trace, warn};

mod connection;
mod endpoint;
mod error;
mod socket;
mod stream;
mod stream_map;
pub use connection::{Outbound, QuicMuxer as Muxer, Substream};
pub use endpoint::{Config, Endpoint, JoinHandle, Listener};
pub use error::Error;
pub use stream_map::Upgrade;
