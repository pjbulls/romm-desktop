//! The copies of one game (USA / Europe / Japan, revisions, translations...) and how to tell
//! them apart.
//!
//! RomM decides which copy represents a game. With `group_by_meta_id` the list holds one ROM per
//! group, chosen by (1) the account's own "default" copy (`is_main_sibling`), (2) a full release
//! over a pre-release, (3) the server's configured region priority, (4) file name. The client
//! has no say in that and doesn't try to repeat it: the ROM a list entry carries *is* the
//! primary, and its `sibling_roms` are the others.
//!
//! RomM describes a sibling by five fields only (see [`SiblingRom`]), with no region or
//! revision, so the label shown for each copy is read from its file name, the way RomM's own
//! version switcher and Argosy's variant chooser do: the trailing `(USA) (Rev 1)` groups.
//!
//! Every copy stays its own ROM (own id, own install, own saves and states), as in RomM and
//! Argosy; nothing here merges them.

use std::collections::HashSet;

use crate::api::types::{Rom, SiblingRom};

/// What a copy is, used only to list real releases before the oddities. Order matters: the
/// derived `Ord` is the display order (same as Argosy's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VersionKind {
    Release,
    /// A file name containing `(demo`, `(beta`, `(proto`, `(sample`, `(kiosk` or `(preview`:
    /// RomM's own pre-release rule (`PRERELEASE_FILENAME_TAGS`).
    PreRelease,
    Translation,
    Hack,
}

/// One copy of a game, as offered in the version picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub id: i64,
    /// Short and distinguishing, e.g. `Europe · En,Fr,De · Rev 1`.
    pub label: String,
    pub kind: VersionKind,
    /// The copy RomM chose to represent the game; the one the library shows.
    pub is_default: bool,
}

const PRERELEASE_TAGS: &[&str] = &["demo", "beta", "proto", "sample", "kiosk", "preview"];

/// The trailing run of `(…)` / `[…]` groups of a name without extension, in order and without
/// the brackets. Same rule as RomM's `fs_name_no_tags`: only groups at the end count, and a
/// group doesn't nest, so `Game (Part 1) Remake` has none.
pub fn tags(name: &str) -> Vec<String> {
    let mut rest = name.trim_end();
    let mut out = Vec::new();
    loop {
        let (open, close) = match rest.chars().next_back() {
            Some(')') => ('(', ')'),
            Some(']') => ('[', ']'),
            _ => break,
        };
        // All four characters are ASCII, so these byte offsets are always char boundaries.
        let body = &rest[..rest.len() - 1];
        let Some(start) = body.rfind(open) else { break };
        let inner = &body[start + 1..];
        if inner.contains(close) {
            break;
        }
        out.push(inner.trim().to_string());
        rest = body[..start].trim_end();
    }
    out.reverse();
    out.retain(|t| !t.is_empty());
    out
}

/// Drops a trailing file extension: up to five letters or digits after the last dot. A folder
/// like `Game v1.1 (USA)` has none, because what follows its last dot isn't one.
fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => {
            let ext = &name[i + 1..];
            if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                &name[..i]
            } else {
                name
            }
        }
        _ => name,
    }
}

/// `Europe · En,Fr,De · Rev 1` for a name without extension, or None when it carries no tags.
/// `[!]` (a verified-dump mark that says nothing about the version) is left out.
pub fn tag_label(name_no_ext: &str) -> Option<String> {
    let t: Vec<String> = tags(name_no_ext).into_iter().filter(|t| t != "!").collect();
    (!t.is_empty()).then(|| t.join(" · "))
}

/// [`tag_label`] for what's on disk: an installed file (`Game (USA).sfc`) or folder.
pub fn tag_label_of_path_name(file_name: &str) -> Option<String> {
    tag_label(strip_extension(file_name))
}

fn kind_of(name_no_ext: &str) -> VersionKind {
    let tokens: Vec<String> = tags(name_no_ext)
        .iter()
        .flat_map(|g| g.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .collect();
    // A translation is never a hack, even when it carries a hack tag too (Argosy's rule).
    if tokens.iter().any(|t| t == "translation") {
        return VersionKind::Translation;
    }
    if tokens.iter().any(|t| t == "hack" || t.starts_with("patched-")) {
        return VersionKind::Hack;
    }
    let lower = name_no_ext.to_ascii_lowercase();
    if PRERELEASE_TAGS.iter().any(|t| lower.contains(&format!("({t}"))) {
        return VersionKind::PreRelease;
    }
    VersionKind::Release
}

fn name_of_rom(r: &Rom) -> String {
    if !r.fs_name_no_ext.is_empty() {
        r.fs_name_no_ext.clone()
    } else {
        strip_extension(&r.fs_name).to_string()
    }
}

fn name_of_sibling(s: &SiblingRom) -> String {
    if !s.fs_name_no_ext.is_empty() {
        s.fs_name_no_ext.clone()
    } else {
        s.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("ROM {}", s.id))
    }
}

/// Every copy of `rom`'s game, `rom` itself (marked default) first, then releases before
/// pre-releases, translations and hacks, each group by label. Empty when the game has only
/// this one copy, so `is_empty()` means "nothing to choose".
pub fn versions(rom: &Rom) -> Vec<Version> {
    let mut seen = HashSet::from([rom.id]);
    let mut raw: Vec<(i64, String, bool)> = vec![(rom.id, name_of_rom(rom), true)];
    for s in &rom.sibling_roms {
        if seen.insert(s.id) {
            raw.push((s.id, name_of_sibling(s), false));
        }
    }
    if raw.len() < 2 {
        return Vec::new();
    }

    let labels: Vec<String> = raw.iter().map(|(_, name, _)| tag_label(name).unwrap_or_else(|| name.clone())).collect();
    // Two copies that read the same ("USA" twice: two dumps, or an unrelated game RomM chained
    // in through a shared provider id) get their whole file names so they can be told apart.
    let count = |l: &str| labels.iter().filter(|x| x.eq_ignore_ascii_case(l)).count();
    let mut out: Vec<Version> = raw
        .iter()
        .zip(&labels)
        .map(|((id, name, is_default), label)| Version {
            id: *id,
            label: if count(label) > 1 { name.clone() } else { label.clone() },
            kind: kind_of(name),
            is_default: *is_default,
        })
        .collect();
    // File names are unique per folder, but two folders could still collide; the id settles it.
    let finally = |l: &str, out: &[Version]| out.iter().filter(|v| v.label.eq_ignore_ascii_case(l)).count();
    let snapshot = out.clone();
    for v in out.iter_mut() {
        if finally(&v.label, &snapshot) > 1 {
            v.label = format!("{} (#{})", v.label, v.id);
        }
    }

    out.sort_by(|a, b| {
        (!a.is_default, a.kind, a.label.to_lowercase(), a.id).cmp(&(!b.is_default, b.kind, b.label.to_lowercase(), b.id))
    });
    out
}

/// For the Installed list: when two installed entries are the same game (same platform and
/// title: USA and Japan both on disk), what tells each apart: the tags of its file name, or the
/// whole name when it has none. `None` for an entry that is the only one of its game, which
/// needs no label. One result per entry, in order. Entries are `(platform, title, file name)`.
pub fn installed_labels(entries: &[(&str, &str, &str)]) -> Vec<Option<String>> {
    let key = |p: &str, t: &str| (p.to_lowercase(), t.to_lowercase());
    let mut counts: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
    for (p, t, _) in entries {
        *counts.entry(key(p, t)).or_default() += 1;
    }
    entries
        .iter()
        .map(|(p, t, file)| {
            (counts[&key(p, t)] > 1).then(|| tag_label_of_path_name(file).unwrap_or_else(|| (*file).to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rom(id: i64, fs_no_ext: &str, siblings: &[(i64, &str)]) -> Rom {
        serde_json::from_value(json!({
            "id": id, "name": "Some Game", "fs_name": format!("{fs_no_ext}.gba"), "fs_name_no_ext": fs_no_ext,
            "platform_id": 1, "platform_slug": "gba", "platform_fs_slug": "gba",
            "sibling_roms": siblings.iter().map(|(i, n)| json!({
                "id": i, "name": "Some Game", "fs_name_no_tags": "Some Game", "fs_name_no_ext": n, "is_main_sibling": false
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn reads_only_the_trailing_tag_groups() {
        assert_eq!(tags("Pokemon - Ruby Version (USA) (Rev 1)"), ["USA", "Rev 1"]);
        assert_eq!(tags("Game (Europe) (En,Fr,De) [!]"), ["Europe", "En,Fr,De", "!"]);
        assert_eq!(tags("Plain Name"), Vec::<String>::new());
        assert_eq!(tags("Game (Part 1) Remake"), Vec::<String>::new(), "a group in the middle is part of the title");
        assert_eq!(tags("Game (Disc 1) (USA)"), ["Disc 1", "USA"]);
        assert_eq!(tags("Ōkami (Japan)"), ["Japan"]);
        assert_eq!(tags("Game ()"), Vec::<String>::new());
    }

    #[test]
    fn malformed_groups_are_ignored_not_fatal() {
        assert!(tags("Game (USA").is_empty());
        assert!(tags("Game USA)").is_empty());
        assert!(tags("Game (a) b)").is_empty());
        assert!(tags("").is_empty());
        assert!(tags(")").is_empty());
    }

    #[test]
    fn label_joins_groups_and_hides_the_verified_dump_mark() {
        assert_eq!(tag_label("Game (Europe) (En,Fr,De) [!]").as_deref(), Some("Europe · En,Fr,De"));
        assert_eq!(tag_label("Game [!]"), None);
        assert_eq!(tag_label("Game"), None);
    }

    #[test]
    fn installed_names_may_carry_an_extension_or_be_folders() {
        assert_eq!(tag_label_of_path_name("Game (USA).sfc").as_deref(), Some("USA"));
        assert_eq!(tag_label_of_path_name("Game v1.1 (USA)").as_deref(), Some("USA"));
        assert_eq!(tag_label_of_path_name("Game (Japan) (Rev 1).gba").as_deref(), Some("Japan · Rev 1"));
        assert_eq!(tag_label_of_path_name("Game.gba"), None);
    }

    #[test]
    fn classifies_like_romm_and_argosy() {
        assert_eq!(kind_of("Game (USA)"), VersionKind::Release);
        assert_eq!(kind_of("Game (USA) (Beta)"), VersionKind::PreRelease);
        assert_eq!(kind_of("Game (Prototype)"), VersionKind::PreRelease);
        assert_eq!(kind_of("Game (Demo Kiosk)"), VersionKind::PreRelease);
        assert_eq!(kind_of("Game (USA) (Translation)"), VersionKind::Translation);
        assert_eq!(kind_of("Game (Hack)"), VersionKind::Hack);
        assert_eq!(kind_of("Game (patched-v2)"), VersionKind::Hack);
        assert_eq!(kind_of("Game (Hack, Translation)"), VersionKind::Translation);
        assert_eq!(kind_of("Betamax (USA)"), VersionKind::Release, "only a tag counts, not the word in a title");
    }

    #[test]
    fn a_game_with_one_copy_has_nothing_to_choose() {
        assert!(versions(&rom(1, "Game (USA)", &[])).is_empty());
        // ...even if RomM lists the ROM as its own sibling.
        assert!(versions(&rom(1, "Game (USA)", &[(1, "Game (USA)")])).is_empty());
    }

    #[test]
    fn lists_the_primary_first_then_releases_before_oddities() {
        let r = rom(
            10,
            "Game (Europe) (En,Fr,De)",
            &[(11, "Game (USA)"), (12, "Game (Japan)"), (13, "Game (USA) (Beta)"), (14, "Game (Hack)"), (15, "Game (USA) (Translation)")],
        );
        let v = versions(&r);
        let got: Vec<(i64, &str, bool)> = v.iter().map(|v| (v.id, v.label.as_str(), v.is_default)).collect();
        assert_eq!(
            got,
            [
                (10, "Europe · En,Fr,De", true),
                (12, "Japan", false),
                (11, "USA", false),
                (13, "USA · Beta", false),
                (15, "USA · Translation", false),
                (14, "Hack", false),
            ]
        );
    }

    #[test]
    fn only_the_primary_is_marked_default() {
        let v = versions(&rom(5, "Game (Japan)", &[(6, "Game (USA)")]));
        assert_eq!(v.iter().filter(|v| v.is_default).count(), 1);
        assert_eq!(v[0].id, 5);
    }

    #[test]
    fn copies_that_would_read_the_same_show_their_file_names() {
        let v = versions(&rom(1, "Game (USA)", &[(2, "Game Deluxe (USA)"), (3, "Game (Japan)")]));
        let l: Vec<&str> = v.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(l, ["Game (USA)", "Game Deluxe (USA)", "Japan"]);
    }

    #[test]
    fn untagged_copies_fall_back_to_the_file_name() {
        let v = versions(&rom(1, "game", &[(2, "game_v2")]));
        assert_eq!(v.iter().map(|v| v.label.as_str()).collect::<Vec<_>>(), ["game", "game_v2"]);
    }

    #[test]
    fn a_sibling_listed_twice_appears_once() {
        let v = versions(&rom(1, "Game (USA)", &[(2, "Game (Japan)"), (2, "Game (Japan)")]));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn identical_names_are_still_told_apart_by_id() {
        let v = versions(&rom(1, "Game (USA)", &[(2, "Game (USA)")]));
        assert_eq!(v[0].label, "Game (USA) (#1)");
        assert_eq!(v[1].label, "Game (USA) (#2)");
    }

    #[test]
    fn installed_copies_of_one_game_are_labelled_and_lone_ones_are_not() {
        let got = installed_labels(&[
            ("gba", "Pokemon Ruby", "Pokemon Ruby (USA).gba"),
            ("gba", "pokemon ruby", "Pokemon Ruby (Japan) (Rev 1).gba"),
            ("gba", "Metroid Fusion", "Metroid Fusion (USA).gba"),
            ("nds", "Pokemon Ruby", "Pokemon Ruby.nds"),
        ]);
        assert_eq!(got, [Some("USA".to_string()), Some("Japan · Rev 1".to_string()), None, None]);
    }

    #[test]
    fn an_untagged_duplicate_is_labelled_with_its_file_name() {
        let got = installed_labels(&[("gba", "Game", "Game.gba"), ("gba", "Game", "Game (USA).gba")]);
        assert_eq!(got, [Some("Game.gba".to_string()), Some("USA".to_string())]);
    }

    #[test]
    fn nothing_installed_is_nothing_labelled() {
        assert!(installed_labels(&[]).is_empty());
    }
}
