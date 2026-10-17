//! Which emulator plays which RomM platform, and which choices make sense for each.
//!
//! Two things are offered for a platform and nothing else: standalone emulators whose
//! profile lists the platform, and RetroArch cores that are listed for it in
//! [`retroarch::CORES`]. So Game Boy offers Game Boy cores and never an Xbox or PlayStation
//! emulator, and the same rules reject a nonsensical choice when one is saved.

use std::collections::BTreeMap;

use crate::profiles;
use crate::retroarch;

const RETROARCH: &str = "retroarch";

/// One thing a user can pick for a platform: a standalone emulator, or RetroArch with a core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// Profile id (`dolphin`, `retroarch`, …).
    pub emulator: String,
    /// Libretro core; only meaningful for RetroArch.
    pub core: Option<String>,
}

impl Choice {
    fn standalone(id: &str) -> Self {
        Choice { emulator: id.to_string(), core: None }
    }

    fn core(core: &str) -> Self {
        Choice { emulator: RETROARCH.to_string(), core: Some(core.to_string()) }
    }

    /// Compact form shared by the UI and the config setters: `dolphin`, `retroarch:gambatte`.
    pub fn value(&self) -> String {
        match &self.core {
            Some(c) => format!("{}:{c}", self.emulator),
            None => self.emulator.clone(),
        }
    }

    pub fn parse(value: &str) -> Choice {
        match value.split_once(':') {
            Some((emulator, core)) => Choice { emulator: emulator.to_string(), core: Some(core.to_string()) },
            None => Choice { emulator: value.to_string(), core: None },
        }
    }

    /// `Dolphin`, `RetroArch · Gambatte`.
    pub fn label(&self) -> String {
        let name = profiles::get(&self.emulator).map(|p| p.name).unwrap_or_else(|| self.emulator.clone());
        match &self.core {
            Some(c) => format!("{name} · {}", retroarch::core_label(c)),
            None => name,
        }
    }
}

/// Everything that can play `slug`: standalone emulators that claim it, then the RetroArch
/// cores listed for it. The first entry is what "Automatic" picks. Empty means nothing here
/// can play the platform yet.
pub fn choices_for(slug: &str) -> Vec<Choice> {
    let mut out: Vec<Choice> = profiles::builtin()
        .into_iter()
        .filter(|p| p.id != RETROARCH && p.platforms.iter().any(|s| s == slug))
        .map(|p| Choice::standalone(&p.id))
        .collect();
    out.extend(retroarch::cores_for(slug).iter().map(|c| Choice::core(c)));
    out
}

/// Can this emulator play this platform at all?
pub fn can_play(slug: &str, emulator: &str) -> bool {
    if emulator == RETROARCH {
        return !retroarch::cores_for(slug).is_empty();
    }
    profiles::get(emulator).is_some_and(|p| p.platforms.iter().any(|s| s == slug))
}

/// What "Automatic" resolves to: a profile that claims the platform, else RetroArch if it
/// has a core for it.
fn automatic(slug: &str) -> Option<String> {
    choices_for(slug).into_iter().next().map(|c| c.emulator)
}

/// Resolution order: the user's choice → automatic. A saved choice that can't play the
/// platform (e.g. left over from when any emulator could be assigned to any system) is
/// ignored rather than launching the wrong emulator.
pub fn emulator_for(slug: &str, overrides: &BTreeMap<String, String>) -> Option<String> {
    if let Some(id) = overrides.get(slug).filter(|id| can_play(slug, id)) {
        return Some(id.clone());
    }
    automatic(slug)
}

/// What the config currently says about a platform.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Saved {
    /// A usable explicit choice; `None` means Automatic.
    pub choice: Option<Choice>,
    /// A saved emulator that can't play this platform, so it's being ignored.
    pub ignored: Option<String>,
}

pub fn saved(slug: &str, emulators: &BTreeMap<String, String>, cores: &BTreeMap<String, String>) -> Saved {
    match emulators.get(slug) {
        Some(id) if can_play(slug, id) => {
            let core = (id == RETROARCH)
                .then(|| cores.get(slug).cloned().or_else(|| retroarch::default_core(slug).map(str::to_string)))
                .flatten();
            Saved { choice: Some(Choice { emulator: id.clone(), core }), ignored: None }
        }
        Some(id) => Saved { choice: None, ignored: Some(id.clone()) },
        None => {
            // Automatic, but a core may still have been picked (by hand in the config file).
            let by_hand = (automatic(slug).as_deref() == Some(RETROARCH)).then(|| cores.get(slug)).flatten();
            Saved { choice: by_hand.map(|c| Choice::core(c)), ignored: None }
        }
    }
}

/// Validates and applies a choice (`None` = back to Automatic), keeping the emulator and core
/// maps consistent. On error nothing is changed.
pub fn set_choice(
    emulators: &mut BTreeMap<String, String>,
    cores: &mut BTreeMap<String, String>,
    slug: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        emulators.remove(slug);
        cores.remove(slug);
        return Ok(());
    };
    let choice = Choice::parse(value);
    let refuse = |c: &Choice| format!("{} can't play '{slug}' games", c.label());

    if choice.emulator == RETROARCH {
        let core = choice
            .core
            .clone()
            .or_else(|| retroarch::default_core(slug).map(str::to_string))
            .filter(|c| retroarch::cores_for(slug).contains(&c.as_str()))
            .ok_or_else(|| refuse(&choice))?;
        emulators.insert(slug.to_string(), RETROARCH.to_string());
        cores.insert(slug.to_string(), core);
    } else {
        if choice.core.is_some() || !can_play(slug, &choice.emulator) {
            return Err(refuse(&choice));
        }
        emulators.insert(slug.to_string(), choice.emulator);
        cores.remove(slug);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_requested_consoles() {
        let none = BTreeMap::new();
        assert_eq!(emulator_for("ngc", &none).unwrap(), "dolphin");
        assert_eq!(emulator_for("wii", &none).unwrap(), "dolphin");
        assert_eq!(emulator_for("psp", &none).unwrap(), "ppsspp");
        assert_eq!(emulator_for("psx", &none).unwrap(), "duckstation");
        assert_eq!(emulator_for("ps2", &none).unwrap(), "pcsx2");
        assert_eq!(emulator_for("snes", &none).unwrap(), "retroarch");
        assert!(emulator_for("switch", &none).is_none());
    }

    #[test]
    fn maps_3ds_and_xbox_to_their_own_emulators() {
        let none = BTreeMap::new();
        assert_eq!(emulator_for("3ds", &none).unwrap(), "azahar");
        assert_eq!(emulator_for("new-nintendo-3ds", &none).unwrap(), "azahar");
        assert_eq!(emulator_for("xbox", &none).unwrap(), "xemu");
    }

    #[test]
    fn ds_stays_on_retroarch_and_xbox360_is_unclaimed() {
        let none = BTreeMap::new();
        // No standalone DS profile: save sync for DS goes through RetroArch's melonDS core.
        assert_eq!(emulator_for("nds", &none).unwrap(), "retroarch");
        // xemu is original Xbox only.
        assert!(emulator_for("xbox360", &none).is_none());
    }

    #[test]
    fn override_wins() {
        let mut o = BTreeMap::new();
        o.insert("psx".to_string(), "retroarch".to_string());
        assert_eq!(emulator_for("psx", &o).unwrap(), "retroarch");
    }
}

#[cfg(test)]
mod choice_tests {
    use super::*;

    type Map = BTreeMap<String, String>;

    fn values(slug: &str) -> Vec<String> {
        choices_for(slug).iter().map(Choice::value).collect()
    }

    /// Every platform anything knows about: standalone profiles plus RetroArch's table.
    fn all_slugs() -> std::collections::BTreeSet<String> {
        let mut s: std::collections::BTreeSet<String> = retroarch::supported_slugs().map(String::from).collect();
        for p in profiles::builtin() {
            s.extend(p.platforms);
        }
        s
    }

    #[test]
    fn game_boy_offers_only_game_boy_options() {
        assert_eq!(
            values("gb"),
            ["retroarch:gambatte", "retroarch:mgba", "retroarch:sameboy", "retroarch:gearboy", "retroarch:tgbdual", "retroarch:vbam"]
        );
        // No console emulators, and no cores for other systems.
        for v in values("gb") {
            assert!(v.starts_with("retroarch:"), "{v}");
        }
        assert!(!values("gb").iter().any(|v| v.contains("mupen64") || v.contains("flycast") || v.contains("pcsx")));
    }

    #[test]
    fn consoles_with_a_dedicated_emulator_offer_just_that() {
        assert_eq!(values("3ds"), ["azahar"]);
        assert_eq!(values("new-nintendo-3ds"), ["azahar"]);
        assert_eq!(values("xbox"), ["xemu"]);
        assert_eq!(values("ps2"), ["pcsx2"]);
        assert_eq!(values("psp"), ["ppsspp"]);
        assert_eq!(values("ngc"), ["dolphin"]);
    }

    #[test]
    fn ds_offers_the_melonds_cores_and_desmume() {
        assert_eq!(values("nds"), ["retroarch:melonds", "retroarch:melondsds", "retroarch:desmume"]);
    }

    #[test]
    fn ps1_offers_duckstation_first_then_retroarch_cores() {
        assert_eq!(
            values("psx"),
            ["duckstation", "retroarch:pcsx_rearmed", "retroarch:swanstation", "retroarch:mednafen_psx"]
        );
    }

    #[test]
    fn unsupported_platforms_offer_nothing() {
        assert!(choices_for("switch").is_empty());
        assert!(choices_for("xbox360").is_empty());
        assert!(choices_for("not-a-platform").is_empty());
    }

    /// The core guarantee: a standalone emulator is only ever offered for platforms its own
    /// profile lists, whatever gets added later.
    #[test]
    fn no_emulator_is_offered_for_a_platform_it_does_not_play() {
        let slugs = all_slugs();
        for p in profiles::builtin().into_iter().filter(|p| p.id != "retroarch") {
            for slug in &slugs {
                let offered = choices_for(slug).iter().any(|c| c.emulator == p.id);
                assert_eq!(offered, p.platforms.contains(slug), "{} for '{slug}'", p.id);
            }
        }
    }

    #[test]
    fn retroarch_cores_are_only_offered_where_listed() {
        for slug in all_slugs() {
            let offered: Vec<String> =
                choices_for(&slug).into_iter().filter(|c| c.emulator == "retroarch").filter_map(|c| c.core).collect();
            assert_eq!(offered, retroarch::cores_for(&slug), "{slug}");
        }
    }

    #[test]
    fn first_choice_is_what_automatic_picks() {
        let none = Map::new();
        for slug in all_slugs() {
            let first = choices_for(&slug).into_iter().next();
            assert_eq!(emulator_for(&slug, &none), first.as_ref().map(|c| c.emulator.clone()), "{slug}");
            if let Some(Choice { emulator, core: Some(core) }) = first {
                assert_eq!(emulator, "retroarch");
                assert_eq!(Some(core.as_str()), retroarch::default_core(&slug), "{slug}");
            }
        }
    }

    #[test]
    fn every_offered_choice_is_accepted_and_nothing_else_is() {
        for slug in all_slugs() {
            for c in choices_for(&slug) {
                let (mut e, mut k) = (Map::new(), Map::new());
                set_choice(&mut e, &mut k, &slug, Some(&c.value())).unwrap_or_else(|err| panic!("{slug} {}: {err}", c.value()));
                assert!(can_play(&slug, &c.emulator));
            }
        }
        // And across every platform, anything not offered is refused.
        for slug in all_slugs() {
            let offered = values(&slug);
            for p in profiles::builtin() {
                for candidate in [p.id.clone(), format!("{}:gambatte", p.id)] {
                    if candidate == "retroarch" || offered.contains(&candidate) {
                        continue;
                    }
                    let (mut e, mut k) = (Map::new(), Map::new());
                    assert!(set_choice(&mut e, &mut k, &slug, Some(&candidate)).is_err(), "{candidate} accepted for {slug}");
                    assert!(e.is_empty() && k.is_empty(), "a refused choice must not change anything");
                }
            }
        }
    }

    #[test]
    fn invalid_saved_override_is_ignored_not_launched() {
        let mut o = Map::new();
        o.insert("gb".into(), "xemu".into()); // left over from when anything could be assigned anywhere
        assert_eq!(emulator_for("gb", &o).unwrap(), "retroarch");
        o.insert("nds".into(), "no-such-emulator".into());
        assert_eq!(emulator_for("nds", &o).unwrap(), "retroarch");
        // A valid alternative is still honoured.
        o.insert("psx".into(), "retroarch".into());
        assert_eq!(emulator_for("psx", &o).unwrap(), "retroarch");
        // And a platform nothing can play stays unplayable even with an override.
        o.insert("switch".into(), "dolphin".into());
        assert!(emulator_for("switch", &o).is_none());
    }

    #[test]
    fn setting_a_core_records_both_and_switching_to_standalone_clears_the_core() {
        let (mut e, mut k) = (Map::new(), Map::new());
        set_choice(&mut e, &mut k, "psx", Some("retroarch:swanstation")).unwrap();
        assert_eq!((e["psx"].as_str(), k["psx"].as_str()), ("retroarch", "swanstation"));
        set_choice(&mut e, &mut k, "psx", Some("duckstation")).unwrap();
        assert_eq!(e["psx"], "duckstation");
        assert!(!k.contains_key("psx"), "a stale core must not linger");
        set_choice(&mut e, &mut k, "psx", None).unwrap();
        assert!(e.is_empty() && k.is_empty());
        // Bare "retroarch" means its default core for that platform.
        set_choice(&mut e, &mut k, "snes", Some("retroarch")).unwrap();
        assert_eq!(k["snes"], "snes9x");
        // ...and is refused where RetroArch has nothing.
        assert!(set_choice(&mut e, &mut k, "ps2", Some("retroarch")).is_err());
    }

    #[test]
    fn refusal_explains_what_cannot_play_what() {
        let (mut e, mut k) = (Map::new(), Map::new());
        let err = set_choice(&mut e, &mut k, "gb", Some("xemu")).unwrap_err();
        assert_eq!(err, "xemu can't play 'gb' games");
        let err = set_choice(&mut e, &mut k, "gb", Some("retroarch:mupen64plus_next")).unwrap_err();
        assert_eq!(err, "RetroArch · Mupen64Plus-Next can't play 'gb' games");
        assert!(set_choice(&mut e, &mut k, "xbox", Some("dolphin:gambatte")).is_err());
    }

    #[test]
    fn saved_reports_the_effective_selection() {
        let (mut e, mut k) = (Map::new(), Map::new());
        assert_eq!(saved("gb", &e, &k), Saved::default());

        // Older config: RetroArch chosen for PS1 with no core recorded → its default core.
        e.insert("psx".into(), "retroarch".into());
        assert_eq!(saved("psx", &e, &k).choice.unwrap().value(), "retroarch:pcsx_rearmed");

        // A core picked by hand while the platform is on Automatic (RetroArch) still shows.
        k.insert("snes".into(), "bsnes".into());
        assert_eq!(saved("snes", &e, &k).choice.unwrap().value(), "retroarch:bsnes");

        // A core outside the table (hand-edited, e.g. a newer one) is kept, not dropped.
        e.insert("snes".into(), "retroarch".into());
        k.insert("snes".into(), "bsnes_hd_beta".into());
        assert_eq!(saved("snes", &e, &k).choice.unwrap().value(), "retroarch:bsnes_hd_beta");

        // An emulator that can't play the platform is reported as ignored.
        e.insert("gb".into(), "xemu".into());
        let s = saved("gb", &e, &k);
        assert_eq!((s.choice, s.ignored.as_deref()), (None, Some("xemu")));
    }

    #[test]
    fn choice_value_and_label_round_trip() {
        for v in ["dolphin", "retroarch:gambatte", "retroarch"] {
            assert_eq!(Choice::parse(v).value(), v);
        }
        assert_eq!(Choice::parse("dolphin").label(), "Dolphin");
        assert_eq!(Choice::parse("retroarch:gambatte").label(), "RetroArch · Gambatte");
        assert_eq!(Choice::parse("retroarch").label(), "RetroArch");
        assert_eq!(Choice::parse("retroarch:brand_new").label(), "RetroArch · brand_new");
    }
}
