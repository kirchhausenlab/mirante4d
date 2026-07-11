//! Narrow temporary owners for predecessor runtime facts.
//!
//! These owners exist only for the gates recorded in the WP-07B entry. They
//! are intentionally separate fields of the composition shell and must not be
//! wrapped in another mutable runtime aggregate.

pub(crate) mod analysis;
pub(crate) mod dataset;
pub(crate) mod import;
pub(crate) mod project;
pub(crate) mod render;
pub(crate) mod ui;
pub(crate) mod validation;
