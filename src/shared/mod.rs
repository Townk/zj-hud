//! Cross-role primitives shared by every plugin role (bar, search, whichkey).
//!
//! These have no role-specific logic: [`state`] is the session-scoped
//! `SharedState` the roles coordinate through, [`kdl`] holds the lenient config
//! parsing helpers, [`geometry`] the floating-pane placement math, and
//! [`color`]/[`icons`] the rendering primitives.

pub mod color;
pub mod geometry;
pub mod icons;
pub mod kdl;
pub mod state;
