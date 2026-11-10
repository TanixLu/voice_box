//! Audio primitives, APO configuration, EXE/APO contracts and Windows helpers shared by both targets.
//!
//! Workspace conventions (all three crates follow them):
//! - Modules are named for one concern; there are no `utils` grab-bags. A helper lives with
//!   its only user, or in a named module such as [`win`] when several modules need it.
//! - Free functions read well qualified by their module (`win::qpc_frequency`,
//!   `pcm_transport::mapping_name`), so they never repeat the module's name.
//! - Types and constants are usually imported unqualified, so they keep a domain prefix
//!   where a bare name would be ambiguous (`PcmFrame`, `PCM_SAMPLE_RATE`, `ApoConfig`).
//! - Getters have no `get_` prefix.

pub mod apo_config;
pub mod apo_identity;
pub mod dsp;
pub mod pcm_transport;
pub mod win;
