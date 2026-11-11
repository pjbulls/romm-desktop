//! App configuration, directories, and token storage.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::types::DeviceRegistration;
use crate::api::{RommClient, CLIENT_SLUG};
use crate::error::{Error, Result};
use crate::saves::write_atomic;
use crate::states::StateOptions;

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
    /// Emulator settings the app applies at launch: scope (a profile id, or `retroarch:<core>`) →
    /// setting id → chosen value. Anything not listed is left as the emulator has it.
    /// See `emu_settings`.
    pub emulator_settings: BTreeMap<String, BTreeMap<String, String>>,
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
            emulator_settings: BTreeMap::new(),
            sync_states: true,
            max_state_mb: 512,
            conflict_policy: crate::sync::ConflictPolicy::KeepBoth,
        }
    }
}

impl Config {
    /// How states are synced, or `None` when the user has turned that off.
    pub fn state_options(&self) -> Option<StateOptions> {
        self.sync_states.then(|| StateOptions { max_bytes: self.max_state_mb * 1024 * 1024 })
    }

    /// Points this configuration at the server just logged in to. The device registered with a
    /// previous server means nothing to this one, so it is forgotten and registered afresh.
    pub fn set_server(&mut self, url: &str, accept_invalid_certs: bool, device_name: Option<String>) {
        self.server_url = Some(url.to_string());
        self.accept_invalid_certs = accept_invalid_certs;
        self.device_name = device_name;
        self.device_id = None;
    }

    /// A client for the server and token saved at login. The errors say what is missing and leave
    /// it to the caller to say how to log in.
    pub fn client(&self, dirs: &AppDirs) -> Result<RommClient> {
        let url = self.server_url.as_deref().ok_or_else(|| Error::msg("not logged in"))?;
        let token = load_token(dirs).ok_or_else(|| Error::msg("no saved token"))?;
        Ok(RommClient::new(url, Some(token), self.accept_invalid_certs)?.with_process_lock(dirs.api_lock()))
    }
}

/// Registers this machine with the server as a device and returns its id. `name` is what the user
/// chose, else one made from the host name.
pub async fn register_device(client: &RommClient, name: Option<&str>) -> Result<String> {
    let registration = DeviceRegistration {
        name: name.map(str::to_string).unwrap_or_else(default_device_name),
        platform: std::env::consts::OS.to_string(),
        client: CLIENT_SLUG.to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        hostname: hostname(),
        sync_mode: "api".to_string(),
    };
    client
        .register_device(&registration)
        .await
        .map(|d| d.device_id)
        .map_err(|e| Error::msg(format!("registering this device with RomM failed (does the token have the devices.write scope?): {e}")))
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
    /// The first copy of each emulator settings file this app has changed (see `emu_settings`).
    pub fn emulator_config_backups(&self) -> PathBuf {
        self.data_dir.join("emulator-config-backups")
    }

    /// The file the desktop app and the CLI lock, so that they never hit RomM's auth together (they
    /// use the same token). See `RommClient::with_process_lock`.
    pub fn api_lock(&self) -> PathBuf {
        self.data_dir.join("api.lock")
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

    fn dirs_in(t: &tempfile::TempDir) -> AppDirs {
        AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d") }
    }

    #[test]
    fn state_options_follow_the_setting_and_the_size_limit() {
        let on = Config { sync_states: true, max_state_mb: 3, ..Default::default() };
        assert_eq!(on.state_options().map(|o| o.max_bytes), Some(3 * 1024 * 1024));
        assert!(Config { sync_states: false, ..Default::default() }.state_options().is_none());
    }

    #[test]
    fn logging_in_to_a_server_forgets_the_device_of_the_last_one() {
        let mut c = Config { device_id: Some("old".into()), device_name: Some("Old name".into()), ..Default::default() };
        c.set_server("http://romm.lan", true, Some("Deck".into()));
        assert_eq!(c.server_url.as_deref(), Some("http://romm.lan"));
        assert!(c.accept_invalid_certs);
        assert_eq!(c.device_name.as_deref(), Some("Deck"));
        assert_eq!(c.device_id, None, "the old server's device id means nothing to the new one");
        // And no name given means the default is used from then on, not the old one.
        c.set_server("http://romm.lan", false, None);
        assert_eq!(c.device_name, None);
    }

    #[test]
    fn a_client_needs_both_a_server_and_a_token() {
        let t = tempfile::tempdir().unwrap();
        let dirs = dirs_in(&t);
        let none = Config::default();
        assert_eq!(none.client(&dirs).err().expect("no server").to_string(), "not logged in");

        let c = Config { server_url: Some("http://romm.lan".into()), ..Default::default() };
        assert_eq!(c.client(&dirs).err().expect("no token").to_string(), "no saved token");

        store_token(&dirs, "rmm_abc").unwrap();
        assert_eq!(c.client(&dirs).unwrap().base_url(), "http://romm.lan");
    }

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
