//! Remote access to xcb over the shared `hraness/relay` Convex deployment:
//! credential custody under `<state-root>/cloud/`, the pinned envelope
//! crypto, device enrollment (`xcb link`), and the supervisor's relay lane.
//!
//! Every wire value parses through `wire` before it is trusted; private key
//! material lives only in `custody`; `client` is the only module that talks
//! to the network.

/// An unchanged fleet projection is still republished at least this
/// often, so a projection's `updated_at` tracks whether the lane can
/// write rather than whether anything changed. Readers treat a
/// projection older than roughly twice this as stale.
pub const PROJECTION_TOUCH_MS: u64 = 600_000;

pub mod canonical;
pub mod client;
pub mod commands;
pub mod controller;
pub mod crypto;
pub mod custody;
pub mod lane;
pub mod link;
pub mod wire;
