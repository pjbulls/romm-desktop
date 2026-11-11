//! Browsing the fetched library in memory: which games a platform or a search shows, and how
//! many each platform has.
//!
//! The library is fetched whole (see `RommClient::library`), grouped one entry per game, so these
//! counts and results are exact, with no paging to get wrong.

use std::collections::{BTreeSet, HashMap};

use crate::api::types::Rom;

/// The games on `platform_id` (all platforms when None) matching `search`, in the order given.
/// Platforms the user has hidden (by slug, see `Config::hidden_platforms`) show nothing, not even
/// when asked for by id.
///
/// A search is a set of words that must all appear, in any order, in the game's name or in the
/// file name of any of its versions. RomM's own search also finds a game through a version that
/// isn't the one the list shows ("Pokemon Rubin" finds Ruby), and so does this.
pub fn filter<'a>(roms: &'a [Rom], platform_id: Option<i64>, search: &str, hidden: &BTreeSet<String>) -> Vec<&'a Rom> {
    let words: Vec<String> = search.split_whitespace().map(str::to_lowercase).collect();
    roms.iter()
        .filter(|r| platform_id.is_none_or(|p| r.platform_id == p))
        .filter(|r| !hidden.contains(&r.platform_slug))
        .filter(|r| words.is_empty() || matches_all(r, &words))
        .collect()
}

fn matches_all(r: &Rom, words: &[String]) -> bool {
    let mut hay = String::new();
    let mut add = |s: &str| {
        hay.push_str(&s.to_lowercase());
        hay.push('\n');
    };
    add(r.title());
    add(&r.fs_name_no_ext);
    for s in &r.sibling_roms {
        if let Some(n) = &s.name {
            add(n);
        }
        add(&s.fs_name_no_ext);
    }
    words.iter().all(|w| hay.contains(w.as_str()))
}

/// Games per platform id. A platform with none is absent.
pub fn count_by_platform(roms: &[Rom]) -> HashMap<i64, i64> {
    let mut m = HashMap::new();
    for r in roms {
        *m.entry(r.platform_id).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rom(id: i64, platform: i64, name: &str, siblings: &[(i64, &str)]) -> Rom {
        serde_json::from_value(json!({
            "id": id, "name": name, "fs_name": format!("{name}.gba"), "fs_name_no_ext": name,
            "platform_id": platform, "platform_slug": "p", "platform_fs_slug": "p",
            "sibling_roms": siblings.iter().map(|(i, n)| json!({"id": i, "fs_name_no_ext": n})).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn lib() -> Vec<Rom> {
        vec![
            rom(1, 10, "Pokemon Ruby", &[(2, "Pokemon Rubin (Germany)")]),
            rom(3, 10, "Metroid Fusion", &[]),
            rom(4, 20, "Metroid Prime", &[]),
        ]
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn hidden(slugs: &[&str]) -> BTreeSet<String> {
        slugs.iter().map(|s| s.to_string()).collect()
    }

    fn on(mut r: Rom, slug: &str) -> Rom {
        r.platform_slug = slug.into();
        r
    }

    fn ids(v: Vec<&Rom>) -> Vec<i64> {
        v.into_iter().map(|r| r.id).collect()
    }

    #[test]
    fn no_filter_returns_everything_in_order() {
        assert_eq!(ids(filter(&lib(), None, "", &none())), [1, 3, 4]);
        assert_eq!(ids(filter(&lib(), None, "   ", &none())), [1, 3, 4]);
    }

    #[test]
    fn platform_narrows() {
        assert_eq!(ids(filter(&lib(), Some(10), "", &none())), [1, 3]);
        assert_eq!(ids(filter(&lib(), Some(99), "", &none())), Vec::<i64>::new());
    }

    #[test]
    fn search_is_case_insensitive_and_needs_every_word() {
        assert_eq!(ids(filter(&lib(), None, "METROID", &none())), [3, 4]);
        assert_eq!(ids(filter(&lib(), None, "metroid fusion", &none())), [3]);
        assert_eq!(ids(filter(&lib(), None, "fusion metroid", &none())), [3]);
        assert!(filter(&lib(), None, "metroid zelda", &none()).is_empty());
    }

    #[test]
    fn search_finds_a_game_through_a_version_that_is_not_shown() {
        assert_eq!(ids(filter(&lib(), None, "rubin", &none())), [1]);
    }

    #[test]
    fn search_and_platform_combine() {
        assert_eq!(ids(filter(&lib(), Some(20), "metroid", &none())), [4]);
        assert!(filter(&lib(), Some(10), "prime", &none()).is_empty());
    }

    #[test]
    fn counts_games_not_versions() {
        let m = count_by_platform(&lib());
        assert_eq!(m.get(&10), Some(&2));
        assert_eq!(m.get(&20), Some(&1));
        assert_eq!(m.get(&30), None);
    }

    /// Two platforms, by slug: Ruby and Fusion on "gba", Prime on "ngc".
    fn two_platforms() -> Vec<Rom> {
        vec![on(lib().remove(0), "gba"), on(lib().remove(1), "gba"), on(lib().remove(2), "ngc")]
    }

    #[test]
    fn a_hidden_platform_takes_all_its_games_out_of_the_list() {
        let roms = two_platforms();
        assert_eq!(ids(filter(&roms, None, "", &hidden(&["gba"]))), [4]);
        assert_eq!(ids(filter(&roms, None, "", &hidden(&["ngc"]))), [1, 3]);
        assert!(filter(&roms, None, "", &hidden(&["gba", "ngc"])).is_empty());
    }

    #[test]
    fn a_hidden_platform_is_out_of_searches_too_and_the_rest_still_found() {
        let roms = two_platforms();
        // "metroid" is on both platforms; only the visible one answers.
        assert_eq!(ids(filter(&roms, None, "metroid", &none())), [3, 4]);
        assert_eq!(ids(filter(&roms, None, "metroid", &hidden(&["gba"]))), [4]);
        assert!(filter(&roms, None, "ruby", &hidden(&["gba"])).is_empty());
        // Finding a game through one of its other versions doesn't get around it.
        assert!(filter(&roms, None, "rubin", &hidden(&["gba"])).is_empty());
    }

    #[test]
    fn a_hidden_platform_shows_nothing_even_when_asked_for_by_id() {
        let roms = two_platforms();
        assert_eq!(ids(filter(&roms, Some(10), "", &none())), [1, 3]);
        assert!(filter(&roms, Some(10), "", &hidden(&["gba"])).is_empty());
        assert_eq!(ids(filter(&roms, Some(20), "", &hidden(&["gba"]))), [4]);
    }

    #[test]
    fn hiding_a_platform_nobody_has_is_harmless_and_order_is_kept() {
        let roms = two_platforms();
        assert_eq!(ids(filter(&roms, None, "", &hidden(&["no-such-platform"]))), [1, 3, 4]);
    }
}
