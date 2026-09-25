//! Remote access to xcb over the shared `hraness/relay` Convex deployment:
//! credential custody under `<state-root>/cloud/`, the pinned envelope
//! crypto, device enrollment (`xcb link`), and the supervisor's relay lane.
//!
//! Every wire value parses through `wire` before it is trusted; private key
//! material lives only in `custody`; `client` is the only module that talks
//! to the network.

pub mod canonical;
pub mod client;
pub mod controller;
pub mod crypto;
pub mod custody;
pub mod lane;
pub mod link;
pub mod wire;
