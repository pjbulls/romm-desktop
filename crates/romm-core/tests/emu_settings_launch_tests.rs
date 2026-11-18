//! Emulator settings, all the way to a running process: a launch is prepared the way the app and the
//! CLI prepare it, the chosen settings are applied, and a stand-in emulator (a shell script) is
//! actually run. The script records the arguments it was started with and takes a copy of its
//! settings file at the moment it starts, which is how these tests know that a setting reached the
//! emulator, and that a file was changed *before* the emulator looked at it and not after.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use romm_core::config::Config;
use romm_core::paths::{Os, PathCtx};
use romm_core::resolve::Resolver;
use romm_core::state::LocalRom;

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn rom(slug: &str, file: &str) -> LocalRom {
    LocalRom {
        rom_id: 1, name: "Game".into(), platform_slug: slug.into(), path: "/r".into(),
        launch_path: PathBuf::from(format!("/r/{file}")), stem: "game".into(), ..Default::default()
    }
}

fn choose(cfg: &mut Config, scope: &str, setting: &str, value: &str) {
    cfg.emulator_settings.entry(scope.into()).or_default().insert(setting.into(), value.into());
}

#[tokio::test]
async fn dolphin_is_started_with_the_chosen_flags_and_none_of_its_files_are_touched() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("args.txt");
    let exe = script(t.path(), "dolphin-emu", &format!("printf '%s\\n' \"$@\" > {}", out.display()));
    let mut cfg = Config::default();
    cfg.emulator_paths.insert("dolphin".into(), exe);
    choose(&mut cfg, "dolphin", "resolution", "3");
    choose(&mut cfg, "dolphin", "fullscreen", "on");

    let r = Resolver { cfg: &cfg, os: Os::Linux, pctx: PathCtx { home: Some(t.path().into()), xdg_data: Some(t.path().join("share")), ..Default::default() } };
    let mut p = r.prepare(&rom("ngc", "melee.iso")).unwrap();
    assert!(p.tuning.apply(&t.path().join("backups")).is_empty());
    assert!(p.command.status().await.unwrap().success());

    let args: Vec<String> = std::fs::read_to_string(&out).unwrap().lines().map(String::from).collect();
    assert_eq!(
        args,
        ["-C", "Graphics.Settings.InternalResolution=3", "-C", "Dolphin.Display.Fullscreen=True", "-b", "-e", "/r/melee.iso"]
    );
    assert!(!t.path().join("backups").exists() && !t.path().join("share").exists(), "Dolphin needs no file edited");
}

#[tokio::test]
async fn pcsx2_sees_its_edited_settings_file_from_the_first_instant_and_the_original_is_kept() {
    let t = tempfile::tempdir().unwrap();
    let cfg_home = t.path().join("cfg");
    let ini = cfg_home.join("PCSX2/inis/PCSX2.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, "[UI]\nSetupWizardIncomplete = false\n\n[EmuCore/GS]\nupscale_multiplier = 1\nVsync = 0\n").unwrap();

    let (args_out, seen) = (t.path().join("args.txt"), t.path().join("seen.ini"));
    let exe = script(
        t.path(),
        "pcsx2-qt",
        &format!("cp \"{}\" \"{}\"\nprintf '%s\\n' \"$@\" > {}", ini.display(), seen.display(), args_out.display()),
    );
    let mut cfg = Config::default();
    cfg.emulator_paths.insert("pcsx2".into(), exe);
    choose(&mut cfg, "pcsx2", "resolution", "4");
    choose(&mut cfg, "pcsx2", "fullscreen", "on");

    let r = Resolver { cfg: &cfg, os: Os::Linux, pctx: PathCtx { home: Some(t.path().into()), xdg_config: Some(cfg_home), ..Default::default() } };
    let mut p = r.prepare(&rom("ps2", "game.iso")).unwrap();
    // Preparing a launch must not have written anything yet.
    assert!(std::fs::read_to_string(&ini).unwrap().contains("upscale_multiplier = 1"));

    let backups = t.path().join("backups");
    assert!(p.tuning.apply(&backups).is_empty());
    assert!(p.command.status().await.unwrap().success());

    // What the emulator saw at start-up: only the one value changed, everything else as it was.
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "[UI]\nSetupWizardIncomplete = false\n\n[EmuCore/GS]\nupscale_multiplier = 4\nVsync = 0\n"
    );
    // The flag goes ahead of PCSX2's own `-- <game>`, so the game is still the one thing after it.
    let args: Vec<String> = std::fs::read_to_string(&args_out).unwrap().lines().map(String::from).collect();
    assert_eq!(args, ["-fullscreen", "-batch", "--", "/r/game.iso"]);
    // And the file as PCSX2 itself wrote it is kept.
    let kept = std::fs::read_to_string(backups.join("pcsx2/PCSX2.ini.orig")).unwrap();
    assert!(kept.contains("upscale_multiplier = 1"), "{kept}");
}

#[tokio::test]
async fn a_user_who_has_never_run_pcsx2_gets_the_game_started_and_a_message_not_a_made_up_config() {
    let t = tempfile::tempdir().unwrap();
    let args_out = t.path().join("args.txt");
    let exe = script(t.path(), "pcsx2-qt", &format!("printf '%s\\n' \"$@\" > {}", args_out.display()));
    let mut cfg = Config::default();
    cfg.emulator_paths.insert("pcsx2".into(), exe);
    choose(&mut cfg, "pcsx2", "resolution", "4");

    let cfg_home = t.path().join("cfg");
    std::fs::create_dir_all(cfg_home.join("PCSX2")).unwrap(); // its folder exists, its inis/PCSX2.ini does not
    let r = Resolver { cfg: &cfg, os: Os::Linux, pctx: PathCtx { home: Some(t.path().into()), xdg_config: Some(cfg_home.clone()), ..Default::default() } };
    let mut p = r.prepare(&rom("ps2", "game.iso")).unwrap();
    let notes = p.tuning.apply(&t.path().join("backups"));

    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("PCSX2") && notes[0].contains("Start the emulator once"), "{notes:?}");
    assert!(p.command.status().await.unwrap().success(), "the game still starts");
    assert!(!cfg_home.join("PCSX2/inis").exists(), "no settings file was invented (PCSX2 would skip its first-run setup)");
}

#[tokio::test]
async fn a_retroarch_core_option_lands_in_the_per_core_file_retroarch_will_read() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("cfg/retroarch");
    std::fs::create_dir_all(root.join("cores")).unwrap();
    std::fs::create_dir_all(root.join("info")).unwrap();
    std::fs::write(root.join("retroarch.cfg"), "").unwrap();
    std::fs::write(root.join("info/flycast_libretro.info"), "corename = \"Flycast\"\n").unwrap();
    std::fs::write(root.join("cores/flycast_libretro.so"), "").unwrap();
    std::fs::write(root.join("retroarch-core-options.cfg"), "reicast_cable_type = \"VGA\"\n").unwrap();

    let (args_out, seen) = (t.path().join("args.txt"), t.path().join("seen.opt"));
    let opt = root.join("config/Flycast/Flycast.opt");
    let exe = script(
        t.path(),
        "retroarch",
        &format!("cp \"{}\" \"{}\"\nprintf '%s\\n' \"$@\" > {}", opt.display(), seen.display(), args_out.display()),
    );
    let mut cfg = Config::default();
    cfg.emulator_paths.insert("retroarch".into(), exe);
    cfg.platform_emulators.insert("dc".into(), "retroarch".into());
    cfg.retroarch_cores.insert("dc".into(), "flycast".into());
    choose(&mut cfg, "retroarch:flycast", "resolution", "1920x1440");

    let r = Resolver { cfg: &cfg, os: Os::Linux, pctx: PathCtx { home: Some(t.path().into()), xdg_config: Some(t.path().join("cfg")), ..Default::default() } };
    let mut p = r.prepare(&rom("dc", "crazy.gdi")).unwrap();
    assert!(p.tuning.apply(&t.path().join("backups")).is_empty());
    assert!(p.command.status().await.unwrap().success());

    // RetroArch gets its options file seeded from the global one, so the user's other option stays.
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"1920x1440\"\n"
    );
}
