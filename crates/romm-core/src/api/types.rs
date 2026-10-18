//! Wire types for the parts of the RomM API this client uses.
//!
//! Checked against RomM's response schemas (backend/endpoints/responses/*.py)
//! and the device-sync-protocol docs. Every non-essential field is optional or
//! defaulted so a server upgrade that adds or drops rarely-used fields doesn't
//! break deserialisation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Firmware {
    pub id: i64,
    pub platform_id: i64,
    pub file_name: String,
    #[serde(default)]
    pub file_size_bytes: u64,
    #[serde(default)]
    pub md5_hash: String,
    #[serde(default)]
    pub is_verified: bool,
    #[serde(default)]
    pub missing_from_fs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    pub id: i64,
    pub slug: String,
    pub fs_slug: String,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub rom_count: i64,
    #[serde(default)]
    pub firmware: Vec<Firmware>,
}

impl Platform {
    pub fn title(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RomFile {
    pub id: i64,
    pub rom_id: i64,
    pub file_name: String,
    #[serde(default)]
    pub file_size_bytes: u64,
    #[serde(default)]
    pub md5_hash: Option<String>,
    #[serde(default)]
    pub is_top_level: bool,
}

/// One entry of a ROM's `sibling_roms`: another file of the same game on the same platform
/// (a different region, revision, translation...). RomM sends only these five fields, so
/// anything more (regions, revision) has to be read from the file name; see `versions`.
#[derive(Debug, Clone, Deserialize)]
pub struct SiblingRom {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub fs_name_no_tags: String,
    #[serde(default)]
    pub fs_name_no_ext: String,
    #[serde(default)]
    pub is_main_sibling: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rom {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    pub fs_name: String,
    #[serde(default)]
    pub fs_name_no_ext: String,
    #[serde(default)]
    pub fs_size_bytes: u64,
    pub platform_id: i64,
    pub platform_slug: String,
    pub platform_fs_slug: String,
    #[serde(default)]
    pub has_multiple_files: bool,
    #[serde(default)]
    pub files: Vec<RomFile>,
    #[serde(default)]
    pub path_cover_small: Option<String>,
    /// The other versions of this game. When the list was requested with `group_by_meta_id`,
    /// this ROM is the one RomM picked to represent the group (see `versions`).
    #[serde(default)]
    pub sibling_roms: Vec<SiblingRom>,
}

impl Rom {
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.fs_name_no_ext)
    }
}

/// RomM's `LimitOffsetPage` (`total` is null when the caller opts out of the count).
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default)]
    pub total: Option<i64>,
    #[serde(default)]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Save {
    pub id: i64,
    pub rom_id: i64,
    pub file_name: String,
    #[serde(default)]
    pub file_size_bytes: u64,
    #[serde(default)]
    pub emulator: Option<String>,
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
}

/// Response of `POST /api/client-tokens/exchange`.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenExchange {
    pub raw_token: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceRegistration {
    pub name: String,
    pub platform: String,
    pub client: String,
    pub client_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub sync_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    pub device_id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NegotiateSave {
    pub rom_id: i64,
    pub file_name: String,
    pub slot: Option<String>,
    pub emulator: Option<String>,
    /// MD5 hex of the file exactly as it would be uploaded.
    pub content_hash: String,
    /// ISO 8601, UTC.
    pub updated_at: String,
    pub file_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NegotiateRequest {
    pub device_id: String,
    pub saves: Vec<NegotiateSave>,
    pub rom_ids: Vec<i64>,
    pub restore_unlisted: bool,
    pub emulators: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Upload,
    Download,
    Conflict,
    Delete,
    NoOp,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    pub action: Action,
    pub rom_id: i64,
    #[serde(default)]
    pub save_id: Option<i64>,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub emulator: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub server_updated_at: Option<String>,
    #[serde(default)]
    pub server_content_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NegotiateResponse {
    pub session_id: i64,
    #[serde(default)]
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaySession {
    pub rom_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_slot: Option<String>,
    pub start_time: String,
    pub end_time: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompleteRequest {
    pub operations_completed: u32,
    pub operations_failed: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub play_sessions: Vec<PlaySession>,
}

/// Query parameters for `POST /api/saves` / `PUT /api/saves/{id}`.
#[derive(Debug, Clone, Default)]
pub struct UploadParams {
    pub rom_id: i64,
    pub emulator: Option<String>,
    /// `None` = archival save that the server never pairs with another.
    pub slot: Option<String>,
    pub device_id: Option<String>,
    pub session_id: Option<i64>,
    pub content_hash: Option<String>,
    pub file_name: String,
}

/// A save state as listed by `GET /api/states`. RomM exposes no content hash for
/// states, so change detection relies on `updated_at` + size plus our own baseline.
#[derive(Debug, Clone, Deserialize)]
pub struct StateInfo {
    pub id: i64,
    pub rom_id: i64,
    pub file_name: String,
    #[serde(default)]
    pub file_size_bytes: u64,
    #[serde(default)]
    pub emulator: Option<String>,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub missing_from_fs: bool,
}

// ------------------------------------------------------------- RetroAchievements
//
// RomM does not talk to the emulator. It asks retroachievements.org about the user's
// `ra_username` and stores the result, so everything here is read from RomM:
//   * the game's achievement list: `GET /api/roms/{id}` -> `merged_ra_metadata`
//   * what the user has earned:    `GET /api/users/me`  -> `ra_progression`
// Sources: backend/handler/metadata/ra_handler.py (RAMetadata, RAUserProgression).

/// Earned-achievement ids are the achievement's *badge name* (`id=badge_name` in RomM's
/// `get_user_progression`), not its numeric id. RomM's type says `str`, but accept a number too
/// so a server that changes its mind doesn't take the whole panel down.
fn string_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    use serde::de::Error as _;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(D::Error::custom(format!("expected a string or number, got {other}"))),
    }
}

/// One achievement in a game's set.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RaAchievement {
    #[serde(default)]
    pub ra_id: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub points: Option<i64>,
    /// RA's badge name. This is what `RaEarned::id` refers to; not unique (see `achievements.rs`).
    #[serde(default)]
    pub badge_id: Option<String>,
    /// Server-relative (`/assets/romm/resources/...`), already rewritten by RomM.
    #[serde(default)]
    pub badge_path: Option<String>,
    #[serde(default)]
    pub badge_path_lock: Option<String>,
    #[serde(default)]
    pub display_order: Option<i64>,
    /// `progression`, `win_condition`, `missable`, or empty.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RaMetadata {
    #[serde(default)]
    pub achievements: Vec<RaAchievement>,
}

/// The slice of `GET /api/roms/{id}` that matters for achievements.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RomRa {
    #[serde(default)]
    pub ra_id: Option<i64>,
    #[serde(default)]
    pub merged_ra_metadata: Option<RaMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RaEarned {
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub date_hardcore: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RaGameProgress {
    #[serde(default)]
    pub rom_ra_id: Option<i64>,
    #[serde(default)]
    pub max_possible: Option<i64>,
    #[serde(default)]
    pub num_awarded: Option<i64>,
    #[serde(default)]
    pub num_awarded_hardcore: Option<i64>,
    #[serde(default)]
    pub most_recent_awarded_date: Option<String>,
    /// `completed`, `mastered`, `beaten-softcore`, `beaten-hardcore`, or null.
    #[serde(default)]
    pub highest_award_kind: Option<String>,
    #[serde(default)]
    pub earned_achievements: Vec<RaEarned>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RaProgression {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub results: Vec<RaGameProgress>,
}

/// `GET /api/users/me` (scope `me.read`), trimmed to what we use.
#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub id: i64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub ra_username: Option<String>,
    #[serde(default)]
    pub ra_progression: Option<RaProgression>,
}
