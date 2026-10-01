//! DB Core owns Lumvise persistence behind two named entry points.
//!
//! Portable contracts and domain records live in the private `interface` module.
//! The local and centralized implementations are private behind
//! [`LocalPersistence`] and [`CentralizedPersistence`].

#[cfg(test)]
extern crate self as lumvise_db_core;

mod interface;
mod local;
mod resource;

pub use interface::*;
pub use local::LocalPersistence;
pub use resource::CentralizedPersistence;

#[cfg(test)]
pub(crate) use local::{DbCore, PZ_REQUIRED_ENTRIES, PzArchive, SemanticStorage};
