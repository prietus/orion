//! Orion core: data model, backend trait, configuration and conversion of
//! MIME messages into a view ready for the UI.

pub mod backend;
pub mod compose;
pub mod config;
pub mod error;
pub mod html;
pub mod model;
pub mod parse;

pub use backend::Backend;
pub use error::{Error, Result};
