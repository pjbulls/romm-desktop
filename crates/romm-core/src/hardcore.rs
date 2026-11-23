//! Hardcore saves: the marker Argosy puts on them, and whether a desktop session played in
//! hardcore mode.
//!
//! Argosy keeps a game's hardcore saves apart from its casual ones. On servers without snapshot
//! sync (RomM before 5.5, which this client speaks) it does so by *appending a marker to the file
//! it uploads*: `{"h":true,"v":1}`, that JSON's length as a little-endian `u32`, then the eight
//! bytes `ARGOSY\x01\x00`. When Argosy holds any hardcore save of a game it will not apply a server
//! save that lacks the marker; it parks it and asks the player to keep the hardcore save or give it
//! up (`SaveDownloader`, `SaveSyncOrchestrator` in Argosy). So a save that comes down marked must go
//! back up marked, and a save may only be marked when it really was made in hardcore mode, or
//! Argosy's separation means nothing.
//!
//! The marker is part of the file RomM stores. RomM (5.4) hashes a zip by its entries, so a marker
//! after the archive doesn't change that hash; every other save is hashed as the whole file, so
//! there the marker is part of what `content_hash` covers (`unit::content_hash` does both).
//!
//! Whether a desktop session was hardcore is read from the emulator's own settings: its profile's
//! `[hardcore]` rule names the file and the two switches (achievements on, hardcore mode on). An
//! emulator without a rule has no RetroAchievements support and its saves are never marked.

use std::path::PathBuf;

use crate::profiles::{Flag, HardcoreRule, SettingsFormat};
use crate::retroarch;
use crate::route;
use crate::sync::EmuEnv;

/// What every marked file ends with: `ARGOSY`, then `0x01 0x00`.
const MAGIC: [u8; 8] = *b"ARGOSY\x01\x00";
/// The marker's JSON as Argosy writes it.
const JSON: &[u8] = br#"{"h":true,"v":1}"#;
/// The longest JSON Argosy and Sigil accept before they stop believing it is a marker.
const MAX_JSON: usize = 1024;
/// The length field and the magic that follow the JSON.
const TAIL: usize = 4 + MAGIC.len();
/// How many bytes `mark` adds to a save.
pub const MARKER_LEN: usize = JSON.len() + TAIL;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `data` without Argosy's hardcore marker, and whether it had one.
///
/// The rules are those of Argosy's `SaveArchiver.readHardcoreTrailer` and Sigil's
/// `sigil_sync_unmarked_len`: the magic at the very end, a little-endian length before it of at
/// most 1024 bytes that fits in the file, and JSON that contains `"h":true`. Anything else is not a
/// marker, and the data comes back whole.
pub fn split(data: &[u8]) -> (&[u8], bool) {
    if data.len() < TAIL || data[data.len() - MAGIC.len()..] != MAGIC {
        return (data, false);
    }
    let len_at = data.len() - TAIL;
    let json_len = u32::from_le_bytes([data[len_at], data[len_at + 1], data[len_at + 2], data[len_at + 3]]) as usize;
    if json_len > MAX_JSON || json_len > len_at {
        return (data, false);
    }
    let json = &data[len_at - json_len..len_at];
    if !contains(json, br#""h":true"#) {
        return (data, false);
    }
    (&data[..len_at - json_len], true)
}

/// Whether `data` carries the hardcore marker.
pub fn is_marked(data: &[u8]) -> bool {
    split(data).1
}

/// `data` with the hardcore marker after it, as Argosy would upload it. Data that already has one
/// is returned as it is, so a marker is never written twice.
pub fn mark(data: &[u8]) -> Vec<u8> {
    if is_marked(data) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len() + JSON.len() + TAIL);
    out.extend_from_slice(data);
    out.extend_from_slice(JSON);
    out.extend_from_slice(&(JSON.len() as u32).to_le_bytes());
    out.extend_from_slice(&MAGIC);
    out
}

// ---------------------------------------------------------------------------------------------
// Is the emulator playing in hardcore mode?
// ---------------------------------------------------------------------------------------------

/// A setting's value as emulators write booleans (`True`, `true`, `1`, `yes`, `on`), or `None` for
/// anything else.
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// The files that may hold `rule`'s settings, most likely first.
fn settings_files(env: &EmuEnv, rule: &HardcoreRule) -> Vec<PathBuf> {
    // Dolphin may keep its config apart from its data (XDG, Flatpak), in more than one place.
    if env.profile.id == "dolphin" {
        return route::dolphin_config_candidates(&env.data_root, &rule.file);
    }
    vec![env.data_root.join(&rule.file)]
}

/// Whether `text`, the content of an emulator's settings file, has achievements and hardcore mode
/// both switched on. A key the file doesn't set is what the emulator takes it to be.
pub fn evaluate(rule: &HardcoreRule, text: &str) -> bool {
    let values = match rule.format {
        SettingsFormat::Ini => route::ini_section(text, &rule.section),
        SettingsFormat::Cfg => retroarch::parse_cfg(text),
    };
    let on = |f: &Flag| values.get(&f.key).and_then(|v| parse_bool(v)).unwrap_or(f.default);
    on(&rule.enabled) && on(&rule.mode)
}

/// Whether the emulator `env` describes is set to play in RetroAchievements hardcore mode.
///
/// This is read from the settings file as it is now, so it is meant to be asked when a play session
/// has just ended. It is `false` for an emulator without RetroAchievements support, for a settings
/// file that can't be read (the emulator has never run, or it is somewhere unexpected), and when
/// either switch is off. Being logged in to RetroAchievements is not checked: the emulator keeps
/// its login token where this client can't use it.
pub fn session_is_hardcore(env: &EmuEnv) -> bool {
    let Some(rule) = env.profile.hardcore.as_ref() else { return false };
    let Some(text) = settings_files(env, rule).iter().find_map(|p| std::fs::read_to_string(p).ok()) else { return false };
    evaluate(rule, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles;

    fn rule(id: &str) -> HardcoreRule {
        profiles::get(id).unwrap().hardcore.unwrap_or_else(|| panic!("{id} has no hardcore rule"))
    }

    /// The bytes Argosy's `appendHardcoreTrailer` adds.
    fn argosys_trailer() -> Vec<u8> {
        let json = br#"{"h":true,"v":1}"#;
        let mut t = json.to_vec();
        t.extend_from_slice(&(json.len() as u32).to_le_bytes());
        t.extend_from_slice(b"ARGOSY\x01\x00");
        t
    }

    #[test]
    fn mark_writes_exactly_what_argosy_writes() {
        let mut expected = b"SAVE".to_vec();
        expected.extend(argosys_trailer());
        assert_eq!(mark(b"SAVE"), expected);
        assert_eq!(mark(b"SAVE").len(), 4 + 16 + 12);
    }

    #[test]
    fn split_reads_a_marker_back_and_returns_the_save_whole() {
        let marked = mark(b"SAVE DATA");
        assert_eq!(split(&marked), (&b"SAVE DATA"[..], true));
        assert!(is_marked(&marked));
        assert_eq!(split(b"SAVE DATA"), (&b"SAVE DATA"[..], false));
        assert_eq!(split(b""), (&b""[..], false));
    }

    #[test]
    fn a_save_that_is_only_a_marker_is_an_empty_marked_save() {
        assert_eq!(split(&argosys_trailer()), (&b""[..], true));
    }

    #[test]
    fn a_marked_save_is_not_marked_twice() {
        let once = mark(b"SAVE");
        assert_eq!(mark(&once), once);
    }

    #[test]
    fn what_argosy_would_not_call_a_marker_is_not_one() {
        // Wrong magic, magic but no room for the JSON, a casual marker (`h` false), and a length
        // that claims more than 1024 bytes or more than the file holds.
        let mut wrong_magic = mark(b"SAVE");
        *wrong_magic.last_mut().unwrap() = 1;
        let casual = {
            let json = br#"{"h":false,"v":1}"#;
            let mut v = b"SAVE".to_vec();
            v.extend_from_slice(json);
            v.extend_from_slice(&(json.len() as u32).to_le_bytes());
            v.extend_from_slice(b"ARGOSY\x01\x00");
            v
        };
        let too_long = {
            let mut v = b"SAVE".to_vec();
            v.extend_from_slice(&1025u32.to_le_bytes());
            v.extend_from_slice(b"ARGOSY\x01\x00");
            v
        };
        let longer_than_the_file = {
            let mut v = Vec::new();
            v.extend_from_slice(&100u32.to_le_bytes());
            v.extend_from_slice(b"ARGOSY\x01\x00");
            v
        };
        for (name, data) in [("magic", wrong_magic), ("casual", casual), ("1025", too_long), ("beyond the file", longer_than_the_file), ("tiny", b"ARGOSY\x01\x00".to_vec())] {
            assert!(!is_marked(&data), "{name}");
            assert_eq!(split(&data).0, &data[..], "{name}: handed back whole");
        }
    }

    #[test]
    fn the_marker_of_another_json_version_still_counts_when_it_says_hardcore() {
        let json = br#"{"v":2,"h":true,"x":1}"#;
        let mut v = b"SAVE".to_vec();
        v.extend_from_slice(json);
        v.extend_from_slice(&(json.len() as u32).to_le_bytes());
        v.extend_from_slice(b"ARGOSY\x01\x00");
        assert_eq!(split(&v), (&b"SAVE"[..], true));
    }

    // -- which emulators have a rule, and the settings each one reads --------------------------

    #[test]
    fn emulators_with_retroachievements_have_a_rule_and_the_rest_never_mark() {
        for id in ["ppsspp", "duckstation", "pcsx2", "dolphin", "retroarch"] {
            assert!(profiles::get(id).unwrap().hardcore.is_some(), "{id}");
        }
        // No RetroAchievements support in these emulators' sources.
        for id in ["azahar", "xemu", "xenia"] {
            assert!(profiles::get(id).unwrap().hardcore.is_none(), "{id}");
        }
    }

    #[test]
    fn ppsspp_is_hardcore_when_achievements_are_on_and_challenge_mode_is_not_turned_off() {
        let r = rule("ppsspp");
        assert!(evaluate(&r, "[Achievements]\nAchievementsEnable = True\nAchievementsChallengeMode = True\n"));
        // Challenge mode is on unless the file says otherwise (PPSSPP's own default).
        assert!(evaluate(&r, "[Achievements]\nAchievementsEnable = True\n"));
        assert!(!evaluate(&r, "[Achievements]\nAchievementsEnable = True\nAchievementsChallengeMode = False\n"));
        assert!(!evaluate(&r, "[Achievements]\nAchievementsEnable = False\nAchievementsChallengeMode = True\n"));
        // Achievements are off unless the file turns them on.
        assert!(!evaluate(&r, "[Achievements]\nAchievementsChallengeMode = True\n"));
        assert!(!evaluate(&r, "[Graphics]\nInternalResolution = 2\n"));
        // Keys of another section are not these.
        assert!(!evaluate(&r, "[General]\nAchievementsEnable = True\n[Achievements]\n"));
    }

    #[test]
    fn duckstation_and_pcsx2_need_both_switches_and_default_to_off() {
        let ds = rule("duckstation");
        assert!(evaluate(&ds, "[Cheevos]\nEnabled = true\nChallengeMode = true\n"));
        assert!(!evaluate(&ds, "[Cheevos]\nEnabled = true\n"), "DuckStation's challenge mode defaults to off");
        assert!(!evaluate(&ds, "[Cheevos]\nChallengeMode = true\n"));

        let px = rule("pcsx2");
        assert!(evaluate(&px, "[Achievements]\nEnabled = true\nChallengeMode = true\n"));
        assert!(!evaluate(&px, "[Achievements]\nEnabled = true\n"), "PCSX2's challenge mode defaults to off");
        assert!(!evaluate(&px, "[Achievements]\nEnabled = false\nChallengeMode = true\n"));
    }

    #[test]
    fn dolphin_hardcore_defaults_to_on_once_achievements_are_enabled() {
        let r = rule("dolphin");
        assert!(evaluate(&r, "[Achievements]\nEnabled = True\n"));
        assert!(evaluate(&r, "[Achievements]\nEnabled = True\nHardcoreEnabled = True\n"));
        assert!(!evaluate(&r, "[Achievements]\nEnabled = True\nHardcoreEnabled = False\n"));
        assert!(!evaluate(&r, "[Achievements]\nHardcoreEnabled = True\n"));
    }

    #[test]
    fn retroarch_reads_its_quoted_flat_config() {
        let r = rule("retroarch");
        assert!(evaluate(&r, "cheevos_enable = \"true\"\ncheevos_hardcore_mode_enable = \"true\"\n"));
        assert!(evaluate(&r, "cheevos_enable = \"true\"\n"), "hardcore mode defaults to on");
        assert!(!evaluate(&r, "cheevos_enable = \"true\"\ncheevos_hardcore_mode_enable = \"false\"\n"));
        assert!(!evaluate(&r, "cheevos_hardcore_mode_enable = \"true\"\n"), "achievements default to off");
        assert!(!evaluate(&r, "video_fullscreen = \"true\"\n"));
    }

    #[test]
    fn an_unreadable_value_falls_back_to_the_emulators_default() {
        let r = rule("ppsspp");
        assert!(evaluate(&r, "[Achievements]\nAchievementsEnable = True\nAchievementsChallengeMode = maybe\n"));
    }

    fn env(id: &str, root: &std::path::Path) -> EmuEnv {
        EmuEnv { profile: profiles::get(id).unwrap(), data_root: root.to_path_buf(), retroarch: None, core: None }
    }

    #[test]
    fn the_session_is_read_from_the_settings_file_in_the_data_root() {
        let t = tempfile::tempdir().unwrap();
        let e = env("ppsspp", t.path());
        assert!(!session_is_hardcore(&e), "no settings file: nothing says hardcore");

        std::fs::create_dir_all(t.path().join("SYSTEM")).unwrap();
        std::fs::write(t.path().join("SYSTEM/ppsspp.ini"), "[Achievements]\nAchievementsEnable = True\n").unwrap();
        assert!(session_is_hardcore(&e));
        std::fs::write(t.path().join("SYSTEM/ppsspp.ini"), "[Achievements]\nAchievementsEnable = True\nAchievementsChallengeMode = False\n").unwrap();
        assert!(!session_is_hardcore(&e));
    }

    #[test]
    fn dolphin_is_read_from_its_config_folder() {
        let t = tempfile::tempdir().unwrap();
        let e = env("dolphin", t.path());
        std::fs::create_dir_all(t.path().join("Config")).unwrap();
        std::fs::write(t.path().join("Config/RetroAchievements.ini"), "[Achievements]\nEnabled = True\nHardcoreEnabled = True\n").unwrap();
        assert!(session_is_hardcore(&e));
    }

    #[test]
    fn an_emulator_without_a_rule_is_never_hardcore() {
        let t = tempfile::tempdir().unwrap();
        for id in ["azahar", "xemu", "xenia"] {
            assert!(!session_is_hardcore(&env(id, t.path())), "{id}");
        }
    }
}
