//! romm-core: everything a RomM desktop client needs except the UI.

pub mod achievements;
pub mod api;
pub mod browse;
pub mod config;
pub mod detect;
pub mod diagnostics;
pub mod emu_settings;
pub mod error;
pub mod launch;
pub mod library;
pub mod paths;
pub mod platforms;
pub mod profiles;
pub mod report;
pub mod resolve;
pub mod retroarch;
pub mod route;
pub mod saves;
pub mod session;
pub mod state;
pub mod states;
pub mod sync;
pub mod unit;
pub mod versions;
pub mod xbox;
pub mod xenia;

pub use error::{Error, Result};
