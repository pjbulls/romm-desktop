//! App configuration, directories, and token storage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::types::DeviceRegistration;
use crate::api::{RommClient, CLIENT_SLUG};
use crate::error::{Error, Result};
use crate::saves::write_atomic;
use crate::shared::{merge3, FileLock, LOCK_WAIT};
use crate::states::StateOptions;
pub use crate::token_store::{delete_token, load_token, read_token, store_token, TokenStorage};

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
    /// Platforms (by slug) the user has hidden from the game browser: the sidebar, All games,
    /// Installed and search leave them, and all their games, out. Saves of installed games still
    /// sync, and the Emulators page still lists them so they can be shown again.
    pub hidden_platforms: BTreeSet<String>,
    /// Emulator settings the app applies at launch: scope (a profile id, or `retroarch:<core>`) →
    /// setting id → chosen value. Anything not listed is left as the emulator has it.
    /// See `emu_settings`.
    pub emulator_settings: BTreeMap<String, BTreeMap<String, String>>,
    /// Sync save states (large files) in addition to saves.
    pub sync_states: bool,
    /// States bigger than this are skipped, in MiB.
    pub max_state_mb: u64,
    pub conflict_policy: crate::sync::ConflictPolicy,
    /// What the file held when this copy last read or wrote it: the common ancestor that `save`
    /// merges against, so that what the other program (app or CLI) saved meanwhile is kept.
    #[serde(skip)]
    base: Option<serde_json::Value>,
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
            hidden_platforms: BTreeSet::new(),
            emulator_settings: BTreeMap::new(),
            sync_states: true,
            max_state_mb: 512,
            conflict_policy: crate::sync::ConflictPolicy::KeepBoth,
            base: None,
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

    /// Chooses the program that runs `emulator` (a profile id), or goes back to finding it by
    /// itself when `path` is None. An unknown emulator or a path that isn't a file is refused and
    /// nothing changes. The caller saves the configuration.
    pub fn set_emulator_path(&mut self, emulator: &str, path: Option<PathBuf>) -> Result<()> {
        if crate::profiles::get(emulator).is_none() {
            let known: Vec<String> = crate::profiles::builtin().into_iter().map(|p| p.id).collect();
            return Err(Error::msg(format!("unknown emulator '{emulator}' (known: {})", known.join(", "))));
        }
        match path {
            Some(path) => {
                if !path.is_file() {
                    return Err(Error::msg(format!("{} is not a file", path.display())));
                }
                self.emulator_paths.insert(emulator.to_string(), path);
            }
            None => {
                self.emulator_paths.remove(emulator);
            }
        }
        Ok(())
    }

    /// A client for the server and token saved at login. The errors say what is missing and leave
    /// it to the caller to say how to log in.
    pub fn client(&self, dirs: &AppDirs) -> Result<RommClient> {
        let url = self.server_url.as_deref().ok_or_else(|| Error::msg("not logged in"))?;
        let token = match read_token(dirs) {
            Ok(Some(token)) => token,
            Ok(None) => return Err(Error::msg("no saved token")),
            // A token may well be saved, just out of reach (a locked keyring): not the same as none.
            Err(why) => return Err(Error::msg(format!("no saved token ({why})"))),
        };
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
    /// Keep the API token in the OS keychain (see `token_store`) rather than in a file. Off in
    /// portable mode, where everything stays under one folder, and when `ROMM_DESKTOP_KEYCHAIN` is
    /// `off`.
    pub keychain: bool,
}

fn keychain_wanted() -> bool {
    let off = std::env::var("ROMM_DESKTOP_KEYCHAIN").map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"));
    !off.unwrap_or(false)
}

impl AppDirs {
    /// `ROMM_DESKTOP_HOME` makes everything live under one folder (portable mode, tests), the token
    /// included: it is kept in a file there rather than in the OS keychain. Elsewhere the token goes
    /// in the keychain unless `ROMM_DESKTOP_KEYCHAIN` is `off` (also `0`, `false`, `no`), for a
    /// machine that has none and would rather not be told so at every sign-in.
    pub fn discover() -> AppDirs {
        if let Some(h) = std::env::var_os("ROMM_DESKTOP_HOME").filter(|v| !v.is_empty()) {
            let h = PathBuf::from(h);
            return AppDirs { config_dir: h.join("config"), data_dir: h.join("data"), keychain: false };
        }
        let pd = directories::ProjectDirs::from("app", "romm", "romm-desktop")
            .expect("could not determine a home directory");
        AppDirs { config_dir: pd.config_dir().to_path_buf(), data_dir: pd.data_dir().to_path_buf(), keychain: keychain_wanted() }
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
        let mut cfg = match std::fs::read_to_string(dirs.config_file()) {
            Ok(s) => toml::from_str::<Config>(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        cfg.base = cfg.snapshot();
        Ok(cfg)
    }

    /// This configuration as a JSON document: the form `merge3` works on. Everything compared goes
    /// through here, so a setting a file lacks is the same on every side.
    fn snapshot(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }

    /// Writes the configuration, keeping what the other program saved since this one last read or
    /// wrote it: under a lock the file is read again and merged with this copy (see
    /// `shared::merge3`: a setting changed here wins, one changed there stays), and this copy takes
    /// on the result. A change made with the CLI while the app was open used to be undone the next
    /// time the app saved anything.
    pub fn save(&mut self, dirs: &AppDirs) -> Result<()> {
        let path = dirs.config_file();
        let _lock = FileLock::acquire(&path, LOCK_WAIT);
        let mine = serde_json::to_value(&*self).map_err(|e| crate::Error::msg(e.to_string()))?;
        let on_disk = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<Config>(&text).ok())
            .and_then(|cfg| cfg.snapshot());
        let merged = match on_disk {
            Some(theirs) => merge3(self.base.as_ref().unwrap_or(&serde_json::Value::Null), &mine, &theirs),
            None => mine,
        };
        // The merged document came from this type, so it converts back; if it somehow does not, this
        // copy is written as it is rather than not at all.
        let mut next: Config = serde_json::from_value(merged.clone()).unwrap_or_else(|_| self.clone());
        let text = toml::to_string_pretty(&next).map_err(|e| crate::Error::msg(e.to_string()))?;
        write_atomic(&path, text.as_bytes())?;
        next.base = Some(merged);
        *self = next;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emulator_path_is_kept_only_for_a_known_emulator_and_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("dolphin-emu");
        std::fs::write(&exe, b"").unwrap();
        let mut c = Config::default();

        c.set_emulator_path("dolphin", Some(exe.clone())).unwrap();
        assert_eq!(c.emulator_paths.get("dolphin"), Some(&exe));

        let err = c.set_emulator_path("nope", Some(exe.clone())).unwrap_err().to_string();
        assert!(err.starts_with("unknown emulator 'nope' (known: ") && err.contains("dolphin"), "{err}");
        let err = c.set_emulator_path("dolphin", Some(dir.path().join("missing"))).unwrap_err().to_string();
        assert!(err.ends_with("is not a file"), "{err}");
        assert_eq!(c.emulator_paths.get("dolphin"), Some(&exe), "a refused change leaves what was there");

        c.set_emulator_path("dolphin", None).unwrap();
        assert!(c.emulator_paths.is_empty(), "None goes back to detection");
    }

    fn dirs_in(t: &tempfile::TempDir) -> AppDirs {
        AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d"), keychain: false }
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
        let dirs = AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d"), keychain: false };
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

    #[test]
    fn hidden_platforms_are_saved_and_default_to_none() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d"), keychain: false };
        assert!(Config::load(&dirs).unwrap().hidden_platforms.is_empty(), "nothing is hidden until asked");

        let mut c = Config::default();
        c.hidden_platforms.insert("gba".into());
        c.hidden_platforms.insert("arcade".into());
        c.save(&dirs).unwrap();
        assert_eq!(Config::load(&dirs).unwrap().hidden_platforms, ["arcade".to_string(), "gba".to_string()].into());
    }

    #[test]
    fn a_config_from_before_hiding_existed_still_loads() {
        let t = tempfile::tempdir().unwrap();
        let dirs = AppDirs { config_dir: t.path().join("c"), data_dir: t.path().join("d"), keychain: false };
        std::fs::create_dir_all(&dirs.config_dir).unwrap();
        std::fs::write(dirs.config_dir.join("config.toml"), "server_url = \"http://romm.lan\"\nsync_states = false\n").unwrap();
        let c = Config::load(&dirs).unwrap();
        assert_eq!(c.server_url.as_deref(), Some("http://romm.lan"));
        assert!(c.hidden_platforms.is_empty());
    }

    #[test]
    fn a_setting_changed_with_the_cli_survives_the_app_saving_another() {
        let t = tempfile::tempdir().unwrap();
        let dirs = dirs_in(&t);
        let mut first = Config { server_url: Some("http://romm.lan".into()), ..Default::default() };
        first.save(&dirs).unwrap();
        let (mut app, mut cli) = (Config::load(&dirs).unwrap(), Config::load(&dirs).unwrap());

        cli.platform_emulators.insert("psx".into(), "duckstation".into());
        cli.retroarch_cores.insert("snes".into(), "bsnes".into());
        cli.save(&dirs).unwrap();
        app.hidden_platforms.insert("gba".into());
        app.conflict_policy = crate::sync::ConflictPolicy::KeepBoth;
        app.save(&dirs).unwrap();

        let on_disk = Config::load(&dirs).unwrap();
        assert_eq!(on_disk.platform_emulators["psx"], "duckstation", "the CLI's setting is not undone");
        assert_eq!(on_disk.retroarch_cores["snes"], "bsnes");
        assert!(on_disk.hidden_platforms.contains("gba"));
        assert_eq!(on_disk.server_url.as_deref(), Some("http://romm.lan"));
        assert_eq!(app.platform_emulators["psx"], "duckstation", "and the app picks it up");
    }

    #[test]
    fn signing_in_with_the_cli_is_not_undone_by_a_running_app_saving_a_setting() {
        let t = tempfile::tempdir().unwrap();
        let dirs = dirs_in(&t);
        let mut first = Config::default();
        first.set_server("http://old.lan", false, None);
        first.device_id = Some("old-device".into());
        first.save(&dirs).unwrap();
        let (mut app, mut cli) = (Config::load(&dirs).unwrap(), Config::load(&dirs).unwrap());

        cli.set_server("https://new.example.com", false, Some("Desk".into()));
        cli.device_id = Some("new-device".into());
        cli.save(&dirs).unwrap();
        app.max_state_mb = 64; // the app saves something unrelated
        app.save(&dirs).unwrap();

        let on_disk = Config::load(&dirs).unwrap();
        assert_eq!(on_disk.server_url.as_deref(), Some("https://new.example.com"));
        assert_eq!(on_disk.device_id.as_deref(), Some("new-device"), "the old server's device id does not come back");
        assert_eq!(on_disk.max_state_mb, 64);
    }

    #[test]
    fn a_setting_cleared_here_is_cleared_in_the_file() {
        let t = tempfile::tempdir().unwrap();
        let dirs = dirs_in(&t);
        let mut c = Config { server_url: Some("http://romm.lan".into()), device_id: Some("d".into()), ..Default::default() };
        c.save(&dirs).unwrap();
        let mut c = Config::load(&dirs).unwrap();
        c.server_url = None;
        c.device_id = None;
        c.save(&dirs).unwrap();
        let back = Config::load(&dirs).unwrap();
        assert!(back.server_url.is_none() && back.device_id.is_none());
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
