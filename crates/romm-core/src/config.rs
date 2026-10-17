//! App configuration, directories, and token storage.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::saves::write_atomic;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server_url: Option<String>,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub rom_dir: Option<PathBuf>,
    /// Self-hosted servers with self-signed certificates.
    pub accept_invalid_certs: bool,
    /// profile id → executable (overrides auto-detection)
    pub emulator_paths: BTreeMap<String, PathBuf>,
    /// RomM platform slug → profile id (overrides the defaults)
    pub platform_emulators: BTreeMap<String, String>,
    /// RomM platform slug → libretro core name, e.g. "snes" → "bsnes"
    pub retroarch_cores: BTreeMap<String, String>,
    /// profile id → extra command-line args placed before the profile's own
    /// (e.g. duckstation → ["-fullscreen"])
    pub extra_args: BTreeMap<String, Vec<String>>,
    /// Sync save states (large files) in addition to saves.
    pub sync_states: bool,
    /// States bigger than this are skipped, in MiB.
    pub max_state_mb: u64,
    pub conflict_policy: crate::sync::ConflictPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server_url: None,
            device_id: None,
            device_name: None,
            rom_dir: None,
            accept_invalid_certs: false,
            emulator_paths: BTreeMap::new(),
            platform_emulators: BTreeMap::new(),
            retroarch_cores: BTreeMap::new(),
            extra_args: BTreeMap::new(),
            sync_states: true,
            max_state_mb: 512,
            conflict_policy: crate::sync::ConflictPolicy::KeepBoth,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppDirs {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppDirs {
    /// `ROMM_DESKTOP_HOME` makes everything live under one folder (portable mode, tests).
    pub fn discover() -> AppDirs {
        if let Some(h) = std::env::var_os("ROMM_DESKTOP_HOME").filter(|v| !v.is_empty()) {
            let h = PathBuf::from(h);
            return AppDirs { config_dir: h.join("config"), data_dir: h.join("data") };
        }
        let pd = directories::ProjectDirs::from("app", "romm", "romm-desktop")
            .expect("could not determine a home directory");
        AppDirs { config_dir: pd.config_dir().to_path_buf(), data_dir: pd.data_dir().to_path_buf() }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn token_file(&self) -> PathBuf {
        self.config_dir.join("token")
    }
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }
    pub fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }
}

impl Config {
    pub fn load(dirs: &AppDirs) -> Result<Config> {
        match std::fs::read_to_string(dirs.config_file()) {
            Ok(s) => Ok(toml::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, dirs: &AppDirs) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| crate::Error::msg(e.to_string()))?;
        write_atomic(&dirs.config_file(), text.as_bytes())?;
        Ok(())
    }

    pub fn rom_dir(&self) -> PathBuf {
        self.rom_dir.clone().unwrap_or_else(|| {
            directories::UserDirs::new()
                .map(|u| u.home_dir().join("Games").join("RomM"))
                .unwrap_or_else(|| PathBuf::from("RomM"))
        })
    }
}

/// Token storage. For now a 0600 file in the config dir.
/// TODO: OS keychain (Windows Credential Manager / Secret Service) behind a feature flag.
pub fn store_token(dirs: &AppDirs, token: &str) -> Result<()> {
    let path = dirs.token_file();
    write_atomic(&path, token.trim().as_bytes())?;
    restrict_permissions(&path)?;
    Ok(())
}

pub fn load_token(dirs: &AppDirs) -> Option<String> {
    std::fs::read_to_string(dirs.token_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_and_token_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d") };
        assert!(Config::load(&dirs).unwrap().server_url.is_none());

        let mut c = Config { server_url: Some("http://romm.lan".into()), ..Default::default() };
        c.platform_emulators.insert("psx".into(), "retroarch".into());
        c.save(&dirs).unwrap();
        let back = Config::load(&dirs).unwrap();
        assert_eq!(back.server_url.as_deref(), Some("http://romm.lan"));
        assert_eq!(back.platform_emulators["psx"], "retroarch");

        assert!(load_token(&dirs).is_none());
        store_token(&dirs, " rmm_abc\n").unwrap();
        assert_eq!(load_token(&dirs).as_deref(), Some("rmm_abc"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dirs.token_file()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}

/// Machine name for the device list. GUI apps don't inherit the shell's `$HOSTNAME`, so fall back to /etc/hostname.
pub fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn default_device_name() -> String {
    format!("{} ({})", hostname().unwrap_or_else(|| "Desktop".into()), std::env::consts::OS)
}
