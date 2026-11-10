//! Audio primitives, APO configuration, EXE/APO contracts and Windows helpers shared by both targets.
//!
//! Workspace conventions (all three crates follow them):
//! - Modules are named for one concern; there are no `utils` grab-bags. A helper lives with
//!   its only user, or in a named module such as [`win`] when several modules need it.
//! - Free functions are named for how callers write them. One called through its module never
//!   repeats the module's name (`pcm_transport::mapping_name`, `registry::read`); one imported
//!   bare keeps a domain word, as a type does (`pcm_now`, `finite_sample`, `qpc_frequency`).
//! - Types and constants are usually imported unqualified, so they keep a domain prefix
//!   where a bare name would be ambiguous (`PcmFrame`, `PCM_SAMPLE_RATE`, `ApoConfig`).
//! - Methods that act are verbs (`publish`, `poll_shortcuts`); a noun method is a getter, with
//!   no `get_` prefix. COM entry points instead mirror their interface method (`get_latency`).
//! - One concept has one name everywhere: modules, types, variables and message keys.

pub mod apo_config;
pub mod apo_identity;
pub mod dsp;
pub mod pcm_transport;
pub mod win;
