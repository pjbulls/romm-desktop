//! romm-core: everything a RomM desktop client needs except the UI.

pub mod api;
pub mod config;
pub mod detect;
pub mod error;
pub mod launch;
pub mod library;
pub mod paths;
pub mod platforms;
pub mod profiles;
pub mod resolve;
pub mod retroarch;
pub mod saves;
pub mod session;
pub mod state;
pub mod states;
pub mod sync;

pub use error::{Error, Result};
