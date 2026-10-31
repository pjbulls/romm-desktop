//! The save units this tool produces and accepts, checked against what Argosy (through
//! argosy-sigil) produced for the same files.

mod common;

use common::*;
use romm_core::unit::{self, Identity, Route, Shape};


fn psp_identity() -> Identity {
    Identity { platform: "psp".into(), title_id: "ULUS10064".into(), save_id: "ULUS10064".into(), ..Default::default() }
}

fn psp_route(root: &std::path::Path) -> Route {
    Route { layout: "ppsspp_standalone".into(), platform: "psp".into(), root: root.to_path_buf(), options: vec![] }
}

#[test]
fn psp_unit_is_exactly_what_argosy_uploads() {
    let t = tempfile::tempdir().unwrap();
    psp_memory_stick(t.path());

    let u = unit::collect(&psp_route(&t.path().join("PSP")), &psp_identity(), "Crisis Core (USA).iso", None).unwrap().expect("saves exist");

    assert_eq!(u.shape, Shape::Folder);
    assert_eq!(u.artifact, "ULUS10064.zip");
    // Every folder of the game, each under its own name: no SAVEDATA/ prefix, no manifest, and
    // neither the installed game data nor another game's save.
    assert_eq!(
        zip_entries(&u.data).into_iter().filter(|n| !n.ends_with('/')).collect::<Vec<_>>(),
        [
            "ULUS10064DATA00/DATA.BIN",
            "ULUS10064DATA00/ICON0.PNG",
            "ULUS10064DATA00/PARAM.SFO",
            "ULUS10064SETTINGS/OPT.BIN",
            "ULUS10064SETTINGS/PARAM.SFO",
        ]
    );
    assert_eq!(u.content_hash, PSP_HASH);
    // RomM would store the same hash for these bytes.
    assert_eq!(unit::content_hash(&u.data), PSP_HASH);
}

#[test]
fn psp_root_may_be_the_psp_folder_or_the_memory_stick_around_it() {
    let t = tempfile::tempdir().unwrap();
    psp_memory_stick(t.path());
    for root in [t.path().to_path_buf(), t.path().join("PSP")] {
        let u = unit::collect(&psp_route(&root), &psp_identity(), "x.iso", None).unwrap().unwrap();
        assert_eq!(u.content_hash, PSP_HASH, "root {}", root.display());
    }
}

#[test]
fn a_game_with_no_saves_collects_nothing() {
    let t = tempfile::tempdir().unwrap();
    psp_memory_stick(t.path());
    let other = Identity { title_id: "NPJH50000".into(), save_id: "NPJH50000".into(), ..psp_identity() };
    assert!(unit::collect(&psp_route(&t.path().join("PSP")), &other, "x.iso", None).unwrap().is_none());
    // And a root that doesn't exist yet is "no saves", not an error.
    assert!(unit::collect(&psp_route(&t.path().join("nope")), &psp_identity(), "x.iso", None).unwrap().is_none());
}

#[test]
fn restoring_a_unit_recreates_the_save_folders_and_leaves_everything_else_alone() {
    let src = tempfile::tempdir().unwrap();
    psp_memory_stick(src.path());
    let u = unit::collect(&psp_route(&src.path().join("PSP")), &psp_identity(), "x.iso", None).unwrap().unwrap();

    // A different machine: has another game's save and the game's installed data, and a stale
    // file inside the game's own save folder that the incoming unit doesn't have.
    let dst = tempfile::tempdir().unwrap();
    write(dst.path(), "PSP/SAVEDATA/ULES00001DATA00/DATA.BIN", b"theirs");
    write(dst.path(), "PSP/SAVEDATA/ULES00001DATA00/PARAM.SFO", &psp_save_sfo());
    write(dst.path(), "PSP/SAVEDATA/ULUS10064/PARAM.SFO", &psp_install_sfo());
    write(dst.path(), "PSP/SAVEDATA/ULUS10064/DATA.BIN", b"installed");
    write(dst.path(), "PSP/SAVEDATA/ULUS10064DATA00/PARAM.SFO", &psp_save_sfo());
    write(dst.path(), "PSP/SAVEDATA/ULUS10064DATA00/STALE.BIN", b"stale");

    let route = psp_route(&dst.path().join("PSP"));
    unit::restore(&route, &psp_identity(), "x.iso", &u.data, None).unwrap();

    let sd = dst.path().join("PSP/SAVEDATA");
    assert_eq!(std::fs::read(sd.join("ULUS10064DATA00/DATA.BIN")).unwrap(), vec![b'A'; 300]);
    assert_eq!(std::fs::read(sd.join("ULUS10064SETTINGS/OPT.BIN")).unwrap(), vec![b'B'; 120]);
    assert!(!sd.join("ULUS10064DATA00/STALE.BIN").exists(), "a file the unit lacks is dropped, as Argosy does");
    assert_eq!(std::fs::read(sd.join("ULUS10064/DATA.BIN")).unwrap(), b"installed", "installed game data is not a save");
    assert_eq!(std::fs::read(sd.join("ULES00001DATA00/DATA.BIN")).unwrap(), b"theirs", "another game's save is untouched");

    // Collecting back gives the unit we restored, byte-for-hash.
    let again = unit::collect(&route, &psp_identity(), "x.iso", None).unwrap().unwrap();
    assert_eq!(again.content_hash, PSP_HASH);
}

#[test]
fn sync_state_marks_a_save_unchanged_until_it_really_changes() {
    let t = tempfile::tempdir().unwrap();
    psp_memory_stick(t.path());
    let route = psp_route(&t.path().join("PSP"));
    let first = unit::collect(&route, &psp_identity(), "x.iso", None).unwrap().unwrap();
    assert!(first.changed, "no earlier sync: everything is new");

    let second = unit::collect(&route, &psp_identity(), "x.iso", Some(&first.state)).unwrap().unwrap();
    assert!(!second.changed, "same saves, same state: nothing to upload");

    write(t.path(), "PSP/SAVEDATA/ULUS10064DATA00/DATA.BIN", &[b'Q'; 300]);
    let third = unit::collect(&route, &psp_identity(), "x.iso", Some(&first.state)).unwrap().unwrap();
    assert!(third.changed);
    assert_ne!(third.content_hash, first.content_hash);
}

// ---------------------------------------------------------------------------------------------
// The other emulators. Each reference hash below is what argosy-sigil's `collect` returned for the
// byte-identical fixture (see common/mod.rs), through the Python binding of the Argosy-pinned
// build: the same units Argosy uploads for these emulators.
// ---------------------------------------------------------------------------------------------

fn route(layout: &str, platform: &str, root: &std::path::Path) -> Route {
    Route { layout: layout.into(), platform: platform.into(), root: root.to_path_buf(), options: vec![] }
}

fn entries(u: &unit::Unit) -> Vec<String> {
    zip_entries(&u.data).into_iter().filter(|n| !n.ends_with('/')).collect()
}

#[test]
fn gamecube_unit_is_a_flat_zip_of_only_this_games_gci_files() {
    let t = tempfile::tempdir().unwrap();
    dolphin_gc_folder(t.path());
    let id = Identity { platform: "gamecube".into(), title_id: "47414C45".into(), save_id: "GALE".into(), ..Default::default() };
    let u = unit::collect(&route("dolphin_standalone", "gamecube", t.path()), &id, "Melee.iso", None).unwrap().unwrap();
    assert_eq!(u.shape, Shape::Multi);
    assert_eq!(u.artifact, "Melee.zip");
    // Zelda's save sits in the same card folder and stays out.
    assert_eq!(entries(&u), ["01-GALE-melee_opts.gci", "01-GALE-melee_save.gci"]);
    assert_eq!(u.content_hash, GC_HASH);
}

#[test]
fn wii_unit_is_the_titles_data_folder_under_its_code() {
    let t = tempfile::tempdir().unwrap();
    wii_title(t.path());
    let id = Identity { platform: "wii".into(), title_id: "52534245".into(), save_id: "00010000/52534245".into(), ..Default::default() };
    let u = unit::collect(&route("dolphin_standalone", "wii", t.path()), &id, "Wii Sports.wbfs", None).unwrap().unwrap();
    assert_eq!(u.shape, Shape::Folder);
    assert_eq!(entries(&u), ["52534245/data/banner.bin", "52534245/data/rs_save.dat"]);
    assert_eq!(u.content_hash, "04f159dfa73cb6fe2108e5d17d593c78");
}

#[test]
fn ps1_unit_is_one_per_game_raw_card_whether_the_card_is_per_game_or_shared() {
    let id = Identity { platform: "psx".into(), title_id: "SLUS-00839".into(), save_id: "SLUS-00839".into(), ..Default::default() };

    let per_game = tempfile::tempdir().unwrap();
    write(per_game.path(), "memcards/Tony Hawk 2_1.mcd", &ps1_card(&[("BASLUS-00839TH2", 1)]));
    let a = unit::collect(&route("duckstation", "psx", per_game.path()), &id, "Tony Hawk 2.chd", None).unwrap().unwrap();
    assert_eq!(a.shape, Shape::Single);
    assert_eq!(a.data.len(), 128 * 1024);
    assert_eq!(a.content_hash, "70668b15aa2b9a9d3be998c4eb3bfe9f");

    // A shared card with another game's save on it: this game's save is cut out as its own card.
    let shared = tempfile::tempdir().unwrap();
    write(shared.path(), "memcards/shared_card_1.mcd", &ps1_card(&[("BASLUS-00839TH2", 1), ("BASLUS-01041CROSS", 2)]));
    let mut r = route("duckstation", "psx", shared.path());
    r.options = vec![("Card1Type".into(), "Shared".into())];
    let b = unit::collect(&r, &id, "Tony Hawk 2.chd", None).unwrap().unwrap();
    assert_eq!(b.content_hash, a.content_hash, "same save, same unit, however the card is shared");
}

#[test]
fn azahar_unit_holds_save_metadata_and_extdata_but_not_installed_content() {
    let t = tempfile::tempdir().unwrap();
    azahar_sdmc(t.path());
    let id = Identity { platform: "3ds".into(), title_id: "000400000017C400".into(), save_id: "00040000/0017c400".into(), ..Default::default() };
    let u = unit::collect(&route("azahar", "3ds", t.path()), &id, "Game.3ds", None).unwrap().unwrap();
    assert_eq!(u.shape, Shape::Folder);
    assert_eq!(
        entries(&u),
        [
            "00040000/0017c400/data/00000001.metadata",
            "00040000/0017c400/data/00000001/save.bin",
            "00040000/0017c400/extdata/user.bin",
        ]
    );
    assert_eq!(u.content_hash, "ede67431d4954184e1b4b778734d8da0");
    // The artifact name carries a slash; only its extension matters on upload.
    assert_eq!(u.extension(), "zip");
}

#[test]
fn restoring_each_emulators_unit_on_another_machine_gives_back_the_same_unit() {
    // (layout, platform, identity, content name, fixture, destination seeded with, reference)
    struct Case {
        layout: &'static str,
        platform: &'static str,
        id: Identity,
        content: &'static str,
        fixture: fn(&std::path::Path),
        // an empty place the emulator would already have made (a card file, a card folder)
        seed: fn(&std::path::Path),
    }
    let cases = [
        Case {
            layout: "dolphin_standalone",
            platform: "gamecube",
            id: Identity { platform: "gamecube".into(), title_id: "47414C45".into(), save_id: "GALE".into(), ..Default::default() },
            content: "Melee.iso",
            fixture: dolphin_gc_folder,
            seed: |r| std::fs::create_dir_all(r.join("GC/USA/Card A")).unwrap(),
        },
        Case {
            layout: "dolphin_standalone",
            platform: "wii",
            id: Identity { platform: "wii".into(), title_id: "52534245".into(), save_id: "00010000/52534245".into(), ..Default::default() },
            content: "Wii Sports.wbfs",
            fixture: wii_title,
            seed: |_| {},
        },
        Case {
            layout: "duckstation",
            platform: "psx",
            id: Identity { platform: "psx".into(), title_id: "SLUS-00839".into(), save_id: "SLUS-00839".into(), ..Default::default() },
            content: "Tony Hawk 2.chd",
            fixture: |r| write(r, "memcards/Tony Hawk 2_1.mcd", &ps1_card(&[("BASLUS-00839TH2", 1)])),
            seed: |r| write(r, "memcards/Tony Hawk 2_1.mcd", &ps1_card(&[])),
        },
        Case {
            layout: "azahar",
            platform: "3ds",
            id: Identity { platform: "3ds".into(), title_id: "000400000017C400".into(), save_id: "00040000/0017c400".into(), ..Default::default() },
            content: "Game.3ds",
            fixture: azahar_sdmc,
            seed: |_| {},
        },
    ];
    for c in cases {
        let src = tempfile::tempdir().unwrap();
        (c.fixture)(src.path());
        let from = unit::collect(&route(c.layout, c.platform, src.path()), &c.id, c.content, None).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        (c.seed)(dst.path());
        let r = route(c.layout, c.platform, dst.path());
        let restored = unit::restore(&r, &c.id, c.content, &from.data, None).unwrap_or_else(|e| panic!("{}/{}: {e}", c.layout, c.platform));

        let back = unit::collect(&r, &c.id, c.content, Some(&restored.state)).unwrap().unwrap_or_else(|| panic!("{}/{}: nothing after restore", c.layout, c.platform));
        assert_eq!(back.identity_hash, from.identity_hash, "{}/{}: same save after the round trip", c.layout, c.platform);
        assert!(!back.changed, "{}/{}: restore records the state, so nothing looks changed", c.layout, c.platform);
    }
}

#[test]
fn a_standalone_emulator_with_no_files_yet_has_no_save_not_an_error() {
    let t = tempfile::tempdir().unwrap();
    let ps2 = Identity { platform: "ps2".into(), title_id: "SLUS-20370".into(), save_id: "BASLUS-20370".into(), ..Default::default() };
    std::fs::create_dir_all(t.path().join("memcards")).unwrap();
    assert!(unit::collect(&route("pcsx2_standalone", "ps2", t.path()), &ps2, "Kingdom Hearts.iso", None).unwrap().is_none());
}

#[test]
fn on_a_brand_new_machine_restore_creates_the_layout_each_emulator_expects() {
    // The pre-launch pull runs before the emulator has ever started, so nothing exists yet. Each
    // save must still land where the emulator will look for it (not, say, under a bare SAVEDATA/).
    let n3 = "sdmc/Nintendo 3DS/00000000000000000000000000000000/00000000000000000000000000000000";
    struct Fresh {
        layout: &'static str,
        platform: &'static str,
        id: Identity,
        content: &'static str,
        fixture: fn(&std::path::Path),
        /// the emulator's data root, under the empty machine, that doesn't exist yet
        root: &'static str,
        expected: Vec<String>,
    }
    let cases = vec![
        Fresh {
            layout: "ppsspp_standalone",
            platform: "psp",
            id: Identity { platform: "psp".into(), title_id: "ULUS10064".into(), save_id: "ULUS10064".into(), ..Default::default() },
            content: "x.iso",
            fixture: psp_memory_stick,
            root: "data/PSP",
            expected: ["DATA00/DATA.BIN", "DATA00/ICON0.PNG", "DATA00/PARAM.SFO", "SETTINGS/OPT.BIN", "SETTINGS/PARAM.SFO"]
                .iter()
                .map(|f| format!("data/PSP/SAVEDATA/ULUS10064{f}"))
                .collect(),
        },
        Fresh {
            layout: "dolphin_standalone",
            platform: "gamecube",
            id: Identity { platform: "gamecube".into(), title_id: "47414C45".into(), save_id: "GALE".into(), ..Default::default() },
            content: "Melee.iso",
            fixture: dolphin_gc_folder,
            root: "data",
            expected: vec!["data/GC/USA/Card A/01-GALE-melee_opts.gci".into(), "data/GC/USA/Card A/01-GALE-melee_save.gci".into()],
        },
        Fresh {
            layout: "dolphin_standalone",
            platform: "wii",
            id: Identity { platform: "wii".into(), title_id: "52534245".into(), save_id: "00010000/52534245".into(), ..Default::default() },
            content: "w.wbfs",
            fixture: wii_title,
            root: "data",
            expected: vec!["data/Wii/title/00010000/52534245/data/banner.bin".into(), "data/Wii/title/00010000/52534245/data/rs_save.dat".into()],
        },
        Fresh {
            layout: "duckstation",
            platform: "psx",
            id: Identity { platform: "psx".into(), title_id: "SLUS-00839".into(), save_id: "SLUS-00839".into(), ..Default::default() },
            content: "Tony Hawk 2.chd",
            fixture: |p| write(p, "memcards/Tony Hawk 2_1.mcd", &ps1_card(&[("BASLUS-00839TH2", 1)])),
            root: "data",
            expected: vec!["data/memcards/Tony Hawk 2_1.mcd".into()],
        },
        Fresh {
            layout: "azahar",
            platform: "3ds",
            id: Identity { platform: "3ds".into(), title_id: "000400000017C400".into(), save_id: "00040000/0017c400".into(), ..Default::default() },
            content: "g.3ds",
            fixture: azahar_sdmc,
            root: "data",
            expected: vec![
                format!("data/{n3}/extdata/00000000/000017c4/user.bin"),
                format!("data/{n3}/title/00040000/0017c400/data/00000001.metadata"),
                format!("data/{n3}/title/00040000/0017c400/data/00000001/save.bin"),
            ],
        },
    ];
    for Fresh { layout, platform, id, content, fixture, root, expected } in cases {
        let src = tempfile::tempdir().unwrap();
        fixture(src.path());
        let from = unit::collect(&route(layout, platform, src.path()), &id, content, None).unwrap().unwrap();

        let base = tempfile::tempdir().unwrap();
        unit::restore(&route(layout, platform, &base.path().join(root)), &id, content, &from.data, None)
            .unwrap_or_else(|e| panic!("{layout}/{platform}: {e}"));

        let mut landed: Vec<String> = Vec::new();
        let mut stack = vec![base.path().to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    landed.push(p.strip_prefix(base.path()).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        landed.sort();
        assert_eq!(landed, expected, "{layout}/{platform}");
    }
}

#[test]
fn a_games_id_is_read_from_inside_its_disc_image() {
    // The file name carries no id, so it can only come from the image's own UMD_DATA.BIN.
    let t = tempfile::tempdir().unwrap();
    let iso = t.path().join("Game.iso");
    std::fs::write(&iso, psp_disc("ULUS-10064")).unwrap();
    let id = unit::identify(&iso, "psp");
    assert_eq!((id.title_id.as_str(), id.save_id.as_str(), id.raw_serial.as_str()), ("ULUS10064", "ULUS10064", "ULUS-10064"));
    assert_eq!(id.platform, "psp");

    // A disc that isn't a PSP game identifies as nothing, rather than as something wrong.
    let junk = t.path().join("Other.iso");
    std::fs::write(&junk, iso9660(&[("README.TXT", b"hello")])).unwrap();
    assert!(!unit::identify(&junk, "psp").has_id());
}

#[test]
fn dreamcast_saves_on_a_shared_vmu_belong_to_no_game_until_claimed() {
    // Dreamcast (and Sega CD, Saturn) saves carry no game id, so on a shared volume Sigil can't say
    // whose they are. Collecting without a claim finds nothing for the game: without claiming,
    // such saves would silently never sync.
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "vmu_save_A1.bin", &vmu_with_both());
    let id = Identity { platform: "dreamcast".into(), ..Default::default() };
    let r = Route { layout: "flycast".into(), platform: "dreamcast".into(), root: t.path().to_path_buf(), options: vec![] };

    let first = unit::collect_claiming(&r, &id, "Some Game.gdi", None, &[]).unwrap();
    assert!(first.unit.is_none(), "nothing is the game's yet");
    assert_eq!(first.unowned, ["MYGAME_SAV", "OTHER_SAV"]);

    // Claiming one makes it the game's, and the other stays unowned.
    let claimed = unit::collect_claiming(&r, &id, "Some Game.gdi", None, &["MYGAME_SAV".to_string()]).unwrap();
    let u = claimed.unit.expect("the claimed save is now this game's");
    assert_eq!((u.shape, u.data.len(), u.artifact.as_str()), (Shape::Single, 128 * 1024, "vmu_A1.bin"));
    assert_eq!(u.content_hash, VMU_CLAIMED_HASH);
    assert_eq!(claimed.unowned, ["OTHER_SAV"]);

    // The state Sigil returns remembers the claim: it need not be passed again.
    let later = unit::collect_claiming(&r, &id, "Some Game.gdi", Some(&u.state), &[]).unwrap();
    assert_eq!(later.unit.expect("still this game's").content_hash, VMU_CLAIMED_HASH);
    assert_eq!(later.unowned, ["OTHER_SAV"]);
}

#[test]
fn only_systems_whose_saves_carry_no_game_id_have_unowned_saves() {
    for p in ["segacd", "dreamcast", "saturn"] {
        assert!(unit::has_unowned_saves(p), "{p}");
    }
    for p in ["psp", "psx", "gamecube", "gb", "n64"] {
        assert!(!unit::has_unowned_saves(p), "{p}");
    }
}
