//! romm-core: everything a RomM desktop client needs except the UI.

pub mod achievements;
pub mod api;
pub mod browse;
pub mod config;
pub mod detect;
pub mod diagnostics;
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
pub mod versions;

pub use error::{Error, Result};
