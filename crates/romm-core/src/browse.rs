//! Browsing the fetched library in memory: which games a platform or a search shows, and how
//! many each platform has.
//!
//! The library is fetched whole (see `RommClient::library`), grouped one entry per game, so these
//! counts and results are exact, with no paging to get wrong.

use std::collections::{BTreeSet, HashMap};

use crate::api::types::Rom;
use crate::state::LocalRom;

/// What a search asks for: a set of words that must all appear, in any order, in the texts a game
/// can be found by. Case does not matter. It is the one rule for every list of games (the whole
/// library and the installed ones), so a search finds the same game wherever it is typed.
pub struct Search {
    words: Vec<String>,
}

impl Search {
    pub fn new(text: &str) -> Search {
        Search { words: text.split_whitespace().map(str::to_lowercase).collect() }
    }

    /// Nothing to look for: every game matches.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Does every word appear in `texts`? The words may be found in different texts (a name and a
    /// file name), but a word is never found across the end of one and the start of the next.
    pub fn matches<'a>(&self, texts: impl IntoIterator<Item = &'a str>) -> bool {
        if self.words.is_empty() {
            return true;
        }
        let mut hay = String::new();
        for t in texts {
            hay.push_str(&t.to_lowercase());
            hay.push('\n');
        }
        self.words.iter().all(|w| hay.contains(w.as_str()))
    }
}

/// The games on `platform_id` (all platforms when None) matching `search`, in the order given.
/// Platforms the user has hidden (by slug, see `Config::hidden_platforms`) show nothing, not even
/// when asked for by id.
///
/// A search is a set of words that must all appear, in any order, in the game's name or in the
/// file name of any of its versions. RomM's own search also finds a game through a version that
/// isn't the one the list shows ("Pokemon Rubin" finds Ruby), and so does this.
pub fn filter<'a>(roms: &'a [Rom], platform_id: Option<i64>, search: &str, hidden: &BTreeSet<String>) -> Vec<&'a Rom> {
    let search = Search::new(search);
    roms.iter()
        .filter(|r| platform_id.is_none_or(|p| r.platform_id == p))
        .filter(|r| !hidden.contains(&r.platform_slug))
        .filter(|r| search.matches(rom_texts(r)))
        .collect()
}

/// The games of the platforms the user has hidden, in the order given, matching `search` the same way
/// `filter` does. This is the "Hidden platforms" view: the one place these games are listed, since
/// `filter` leaves them out of everything else. Between them the two cover every game exactly once.
pub fn filter_hidden<'a>(roms: &'a [Rom], search: &str, hidden: &BTreeSet<String>) -> Vec<&'a Rom> {
    let search = Search::new(search);
    roms.iter()
        .filter(|r| hidden.contains(&r.platform_slug))
        .filter(|r| search.matches(rom_texts(r)))
        .collect()
}

/// The installed games matching `search`, in the order given, leaving out hidden platforms as
/// `filter` does. The same rule as there: every word, in any order, in the game's name or the name
/// of the file it was installed as.
pub fn filter_installed<'a>(roms: impl IntoIterator<Item = &'a LocalRom>, search: &str, hidden: &BTreeSet<String>) -> Vec<&'a LocalRom> {
    let search = Search::new(search);
    roms.into_iter()
        .filter(|r| !hidden.contains(&r.platform_slug))
        .filter(|r| search.matches([r.name.as_str(), r.stem.as_str()]))
        .collect()
}

/// What a game is found by: its name and file name, and those of its other versions.
fn rom_texts(r: &Rom) -> impl Iterator<Item = &str> {
    [r.title(), r.fs_name_no_ext.as_str()]
        .into_iter()
        .chain(r.sibling_roms.iter().flat_map(|s| s.name.as_deref().into_iter().chain(std::iter::once(s.fs_name_no_ext.as_str()))))
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

    #[test]
    fn the_hidden_view_lists_only_the_hidden_platforms_games_in_order() {
        let roms = two_platforms();
        assert_eq!(ids(filter_hidden(&roms, "", &hidden(&["gba"]))), [1, 3]);
        assert_eq!(ids(filter_hidden(&roms, "", &hidden(&["ngc"]))), [4]);
        assert_eq!(ids(filter_hidden(&roms, "", &hidden(&["gba", "ngc"]))), [1, 3, 4]);
    }

    #[test]
    fn the_hidden_view_is_empty_when_nothing_is_hidden_or_the_platform_has_no_games() {
        let roms = two_platforms();
        assert!(filter_hidden(&roms, "", &none()).is_empty());
        assert!(filter_hidden(&roms, "", &hidden(&["no-such-platform"])).is_empty());
    }

    #[test]
    fn a_search_in_the_hidden_view_only_finds_hidden_games_the_same_way_as_elsewhere() {
        let roms = two_platforms();
        // "metroid" is on both platforms; only the hidden one answers.
        assert_eq!(ids(filter_hidden(&roms, "metroid", &hidden(&["ngc"]))), [4]);
        assert_eq!(ids(filter_hidden(&roms, "METROID fusion", &hidden(&["gba"]))), [3]);
        assert!(filter_hidden(&roms, "prime", &hidden(&["gba"])).is_empty());
        // Through another version's file name, as `filter` finds it.
        assert_eq!(ids(filter_hidden(&roms, "rubin", &hidden(&["gba"]))), [1]);
    }

    /// Hiding moves games between the two lists; it never loses or duplicates one.
    #[test]
    fn the_shown_list_and_the_hidden_view_together_are_every_game_exactly_once() {
        let roms = two_platforms();
        for h in [none(), hidden(&["gba"]), hidden(&["ngc"]), hidden(&["gba", "ngc"]), hidden(&["other"])] {
            for q in ["", "metroid", "ruby"] {
                let mut both = ids(filter(&roms, None, q, &h));
                both.extend(ids(filter_hidden(&roms, q, &h)));
                both.sort();
                assert_eq!(both, ids(filter(&roms, None, q, &none())), "hidden {h:?}, search {q:?}");
            }
        }
    }

    fn local(id: i64, platform: &str, name: &str, stem: &str) -> LocalRom {
        LocalRom { rom_id: id, name: name.into(), platform_slug: platform.into(), stem: stem.into(), ..Default::default() }
    }

    fn installed() -> Vec<LocalRom> {
        vec![
            local(1, "gba", "Pokemon Ruby", "Pokemon Ruby (USA)"),
            local(3, "gba", "Metroid Fusion", "Metroid Fusion"),
            local(4, "ngc", "Metroid Prime", "Metroid Prime"),
        ]
    }

    fn installed_ids(v: Vec<&LocalRom>) -> Vec<i64> {
        v.into_iter().map(|r| r.rom_id).collect()
    }

    /// The Installed list used to be one substring of the name, so the same words in another order
    /// found a game in All games and not here.
    #[test]
    fn installed_search_takes_the_words_in_any_order_like_the_library_does() {
        let roms = installed();
        assert_eq!(installed_ids(filter_installed(&roms, "metroid fusion", &none())), [3]);
        assert_eq!(installed_ids(filter_installed(&roms, "fusion metroid", &none())), [3]);
        assert_eq!(installed_ids(filter_installed(&roms, "METROID", &none())), [3, 4]);
        assert!(filter_installed(&roms, "metroid zelda", &none()).is_empty());
    }

    #[test]
    fn installed_search_also_finds_a_game_by_the_name_of_its_file() {
        let roms = installed();
        assert_eq!(installed_ids(filter_installed(&roms, "usa", &none())), [1]);
    }

    #[test]
    fn installed_search_with_nothing_to_find_lists_everything_in_order() {
        let roms = installed();
        assert_eq!(installed_ids(filter_installed(&roms, "", &none())), [1, 3, 4]);
        assert_eq!(installed_ids(filter_installed(&roms, "   ", &none())), [1, 3, 4]);
    }

    #[test]
    fn installed_games_on_a_hidden_platform_are_not_listed_or_found() {
        let roms = installed();
        assert_eq!(installed_ids(filter_installed(&roms, "", &hidden(&["gba"]))), [4]);
        assert!(filter_installed(&roms, "ruby", &hidden(&["gba"])).is_empty());
    }

    #[test]
    fn a_word_is_not_found_across_the_end_of_one_text_and_the_start_of_the_next() {
        let roms = vec![local(1, "gba", "Poke", "mon")];
        assert!(filter_installed(&roms, "pokemon", &none()).is_empty());
        assert_eq!(installed_ids(filter_installed(&roms, "poke mon", &none())), [1]);
    }
}
