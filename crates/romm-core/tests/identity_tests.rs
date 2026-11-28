//! Does Sigil's identity for a unit match the one RomM computes for the same save?
//!
//! RomM's `identity_hash` of a zip unit is the hash of the unit's only member once its clock members are
//! set aside (`zip_identity_hash`), and a client that disagrees sees every tick of a cart's clock as a change
//! to its save. Sigil caught up with that rule in argosy-sigil `ae27ad2`; this repo vendors a commit that has it.

use romm_core::saves::md5_bytes;
use romm_core::unit::{collect, identify, Route};

/// A 32 KiB Game Boy ROM whose header (cart type 0x10: MBC3 with a timer, RAM and a battery) is valid.
fn clock_cart(path: &std::path::Path) {
    let mut rom = vec![0u8; 0x8000];
    rom[0x134..0x13C].copy_from_slice(b"CLOCKCRT");
    rom[0x147] = 0x10;
    rom[0x148] = 0x00; // 32 KiB ROM
    rom[0x149] = 0x03; // 32 KiB RAM
    let mut x = 0u8;
    for b in &rom[0x134..=0x14C] {
        x = x.wrapping_sub(*b).wrapping_sub(1);
    }
    rom[0x14D] = x;
    std::fs::write(path, rom).unwrap();
}

#[test]
fn a_clock_carts_identity_is_the_hash_of_its_save_ram_as_romm_computes_it() {
    let t = tempfile::tempdir().unwrap();
    let rom = t.path().join("Game.gb");
    clock_cart(&rom);
    let saves = t.path().join("saves");
    std::fs::create_dir_all(&saves).unwrap();
    let sram: Vec<u8> = (0..0x8000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(saves.join("Game.srm"), &sram).unwrap();
    // RetroArch's gambatte writes the clock beside it, as 48 bytes.
    std::fs::write(saves.join("Game.rtc"), [3u8; 48]).unwrap();

    let id = identify(&rom, "gb");
    assert_eq!(id.platform, "gb");
    let route = Route { layout: "gambatte".into(), platform: "gb".into(), root: saves.clone(), options: vec![] };
    let unit = collect(&route, &id, "Game.gb", None).expect("collect").expect("a unit: there is a save");

    assert_eq!(unit.identity_hash, md5_bytes(&sram), "RomM would hash the save RAM alone, the clock set aside");

    // The clock ticking is the case this is for: the content hash moves, the identity must not.
    std::fs::write(saves.join("Game.rtc"), [9u8; 48]).unwrap();
    let ticked = collect(&route, &id, "Game.gb", None).unwrap().unwrap();
    assert_ne!(ticked.content_hash, unit.content_hash, "the clock is in the unit");
    assert_eq!(ticked.identity_hash, unit.identity_hash, "and not in what makes it a different save");
}
