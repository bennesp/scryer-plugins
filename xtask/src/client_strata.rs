//! Which Scryer clients the central catalog is published for.
//!
//! Every supported Scryer (0.21.0 and newer) reads only the modern redirect,
//! `catalog-v3.modern.redirect.json`, and takes the master catalog it points
//! at unfiltered: capability-aware clients skip artifacts they cannot run
//! instead of rejecting the document. There is therefore one publication
//! stratum and no per-band projection.
//!
//! The legacy positional ladder (`catalog-v3.redirect.json`) and its
//! per-stratum projections served only clients below the support floor and
//! are no longer produced. Whatever was last published there stays frozen for
//! those installs.

/// Artifact `runtime` wire values. `wasm32-wasip1` is historical: older
/// releases in the master catalog still carry it.
pub const TARGET_WASIP1: &str = "wasm32-wasip1";
pub const TARGET_WASIP2: &str = "wasm32-wasip2";
