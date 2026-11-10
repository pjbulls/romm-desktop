use super::*;

fn sets(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ------------------------------------------------------------------------------------ INI

#[test]
fn ini_replaces_a_value_and_leaves_everything_else_byte_for_byte() {
    let text = "; kept\n[Graphics]\nFullScreen = False\nInternalResolution = 1   \nVSync = True\n\n[Audio]\nVolume = 5\n";
    let out = patch_ini(text, "Graphics", &sets(&[("InternalResolution", "4")]), false);
    // The value's own spacing after the `=` is kept; the rest of that line (here trailing spaces) is replaced.
    assert_eq!(out, "; kept\n[Graphics]\nFullScreen = False\nInternalResolution = 4\nVSync = True\n\n[Audio]\nVolume = 5\n");
}

#[test]
fn ini_only_touches_the_named_section() {
    let text = "[A]\nscale = 1\n[B]\nscale = 1\n";
    assert_eq!(patch_ini(text, "B", &sets(&[("scale", "9")]), false), "[A]\nscale = 1\n[B]\nscale = 9\n");
}

#[test]
fn ini_adds_a_missing_key_at_the_end_of_its_section_not_the_file() {
    let text = "[Graphics]\nVSync = True\n\n[Audio]\nVolume = 5\n";
    assert_eq!(
        patch_ini(text, "Graphics", &sets(&[("InternalResolution", "3")]), false),
        "[Graphics]\nVSync = True\nInternalResolution = 3\n\n[Audio]\nVolume = 5\n"
    );
}

#[test]
fn ini_adds_a_missing_section_at_the_end() {
    assert_eq!(patch_ini("[A]\nx = 1\n", "B", &sets(&[("y", "2")]), false), "[A]\nx = 1\n\n[B]\ny = 2\n");
}

#[test]
fn ini_handles_a_file_with_no_trailing_newline() {
    assert_eq!(patch_ini("[A]\nx = 1", "A", &sets(&[("y", "2")]), false), "[A]\nx = 1\ny = 2\n");
    assert_eq!(patch_ini("[A]\nx = 1", "B", &sets(&[("y", "2")]), false), "[A]\nx = 1\n\n[B]\ny = 2\n");
    assert_eq!(patch_ini("[A]\nx = 1", "A", &sets(&[("x", "9")]), false), "[A]\nx = 9");
}

#[test]
fn ini_keeps_windows_line_endings_and_adds_new_lines_the_same_way() {
    let text = "[A]\r\nx = 1\r\n[B]\r\nz = 1\r\n";
    let out = patch_ini(text, "A", &sets(&[("x", "2"), ("y", "3")]), false);
    assert_eq!(out, "[A]\r\nx = 2\r\ny = 3\r\n[B]\r\nz = 1\r\n");
    assert!(!out.replace("\r\n", "").contains('\n'), "no bare newline slipped in");
}

#[test]
fn ini_keeps_a_byte_order_mark() {
    let out = patch_ini("\u{feff}[A]\nx = 1\n", "A", &sets(&[("x", "2")]), false);
    assert_eq!(out, "\u{feff}[A]\nx = 2\n");
}

#[test]
fn ini_never_treats_a_comment_as_the_key() {
    let text = "[A]\n; x = 1\n# x = 1\nx = 1\n";
    assert_eq!(patch_ini(text, "A", &sets(&[("x", "7")]), false), "[A]\n; x = 1\n# x = 1\nx = 7\n");
}

#[test]
fn ini_matches_keys_exactly_so_a_longer_name_is_not_mistaken_for_it() {
    let text = "[A]\nscale_extra = 5\nscale = 1\n";
    assert_eq!(patch_ini(text, "A", &sets(&[("scale", "2")]), false), "[A]\nscale_extra = 5\nscale = 2\n");
}

#[test]
fn ini_changes_every_copy_of_a_repeated_key_in_the_section() {
    assert_eq!(patch_ini("[A]\nx = 1\nx = 2\n", "A", &sets(&[("x", "9")]), false), "[A]\nx = 9\nx = 9\n");
}

#[test]
fn ini_keeps_the_unspaced_style_of_qt_files_for_new_keys() {
    let text = "[Renderer]\ngraphics_api=1\n";
    assert_eq!(patch_ini(text, "Renderer", &sets(&[("use_vsync", "true")]), false), "[Renderer]\ngraphics_api=1\nuse_vsync=true\n");
}

/// Azahar (like Citra) ignores `key` while `key\default` is true, so both have to be set; and the
/// two names must not be confused with each other.
#[test]
fn ini_sets_a_qt_value_and_its_default_flag_together() {
    let text = "[Renderer]\ngraphics_api\\default=true\ngraphics_api=1\nresolution_factor\\default=true\nresolution_factor=1\n[Layout]\nlayout_option\\default=true\n";
    let out = patch_ini(text, "Renderer", &sets(&[("resolution_factor\\default", "false"), ("resolution_factor", "3")]), false);
    assert_eq!(
        out,
        "[Renderer]\ngraphics_api\\default=true\ngraphics_api=1\nresolution_factor\\default=false\nresolution_factor=3\n[Layout]\nlayout_option\\default=true\n"
    );
}

#[test]
fn ini_without_sections_quotes_values_the_way_retroarch_does() {
    let text = "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"640x480\"\n";
    let out = patch_ini(text, "", &sets(&[("reicast_internal_resolution", "1920x1440")]), true);
    assert_eq!(out, "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"1920x1440\"\n");
}

#[test]
fn ini_without_sections_adds_to_an_empty_file_and_before_any_first_section() {
    assert_eq!(patch_ini("", "", &sets(&[("k", "v")]), true), "k = \"v\"\n");
    assert_eq!(patch_ini("[S]\na = 1\n", "", &sets(&[("k", "v")]), false), "k = v\n[S]\na = 1\n");
}

#[test]
fn ini_is_idempotent() {
    let once = patch_ini("[A]\nx = 1\n", "A", &sets(&[("x", "2"), ("y", "3")]), false);
    assert_eq!(patch_ini(&once, "A", &sets(&[("x", "2"), ("y", "3")]), false), once);
}

// ------------------------------------------------------------------------------------ TOML

fn path(p: &str) -> Vec<String> {
    p.split('.').map(str::to_string).collect()
}

#[test]
fn toml_changes_one_value_and_keeps_its_comment_and_the_rest() {
    let text = "# xemu\n[general]\nshow_welcome = false\n\n[display.quality]\nsurface_scale = 1 # internal res\n\n[display.window]\nfullscreen_on_startup = false\n";
    let out = patch_toml(text, &path("display.quality.surface_scale"), "3", TomlKind::Int).unwrap();
    assert_eq!(
        out,
        "# xemu\n[general]\nshow_welcome = false\n\n[display.quality]\nsurface_scale = 3 # internal res\n\n[display.window]\nfullscreen_on_startup = false\n"
    );
}

#[test]
fn toml_writes_booleans_and_strings_with_the_right_type() {
    let out = patch_toml("[display.window]\nfullscreen_on_startup = false\n", &path("display.window.fullscreen_on_startup"), "true", TomlKind::Bool).unwrap();
    assert_eq!(out, "[display.window]\nfullscreen_on_startup = true\n");
    let out = patch_toml("[a]\nk = \"x\"\n", &path("a.k"), "y", TomlKind::String).unwrap();
    assert_eq!(out, "[a]\nk = \"y\"\n");
}

#[test]
fn toml_adds_a_missing_key_and_a_missing_table() {
    let out = patch_toml("[display.quality]\nsurface_scale = 1\n", &path("display.window.fullscreen_on_startup"), "true", TomlKind::Bool).unwrap();
    assert!(out.contains("[display.quality]\nsurface_scale = 1\n"), "{out}");
    assert!(out.contains("[display.window]\nfullscreen_on_startup = true"), "{out}");
    assert!(!out.contains("\n[display]\n"), "no empty [display] header is added:\n{out}");
    // And the result still parses as TOML with the value where it belongs.
    let doc: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(doc["display"]["window"]["fullscreen_on_startup"].as_bool(), Some(true));
    assert_eq!(doc["display"]["quality"]["surface_scale"].as_integer(), Some(1));
}

#[test]
fn toml_refuses_text_that_is_not_toml_and_values_of_the_wrong_kind() {
    assert!(patch_toml("[display\nbroken", &path("a.b"), "1", TomlKind::Int).unwrap_err().contains("valid TOML"));
    assert!(patch_toml("", &path("a.b"), "two", TomlKind::Int).unwrap_err().contains("whole number"));
    assert!(patch_toml("", &path("a.b"), "yes", TomlKind::Bool).unwrap_err().contains("true or false"));
    assert!(patch_toml("a = 1\n", &path("a.b"), "1", TomlKind::Int).unwrap_err().contains("table"));
}

// ------------------------------------------------------------------------------------ the catalogue

fn profile(id: &str) -> Profile {
    profiles::get(id).unwrap()
}

/// What a typo in a profile's `[[settings]]` would otherwise only show as a setting that quietly does nothing.
#[test]
fn every_declared_setting_is_well_formed() {
    let mut seen = std::collections::BTreeSet::new();
    for p in profiles::builtin() {
        for s in &p.settings {
            let who = format!("{} / {}", p.id, s.id);
            assert!(!s.id.is_empty() && !s.label.is_empty(), "{who}: needs an id and a label");
            assert!(seen.insert((scope_id(&p.id, s.core.as_deref()), s.id.clone())), "{who}: declared twice for the same scope");
            assert!(!s.choices.is_empty(), "{who}: has no choices");

            // RetroArch settings belong to a known core; nobody else has a core.
            match (&s.core, p.id == "retroarch") {
                (Some(c), true) => assert_ne!(&retroarch::core_label(c), c, "{who}: '{c}' isn't a core this app knows"),
                (None, true) => panic!("{who}: a RetroArch setting has to name its core"),
                (Some(_), false) => panic!("{who}: only RetroArch settings have a core"),
                (None, false) => {}
            }

            let mut values = std::collections::BTreeSet::new();
            for c in &s.choices {
                assert!(!c.value.is_empty() && !c.label.is_empty(), "{who}: a choice needs a value and a label");
                assert!(values.insert(&c.value), "{who}: choice '{}' is listed twice", c.value);
                let effects = if c.apply.is_empty() { &s.apply } else { &c.apply };
                assert!(!effects.is_empty(), "{who}: choice '{}' does nothing", c.value);
                for e in effects {
                    check_effect(&who, &p, s, e);
                }
            }
        }
    }
    assert!(!seen.is_empty(), "the catalogue is empty");
}

fn check_effect(who: &str, p: &Profile, s: &Setting, e: &Apply) {
    let placeholders_ok = |t: &str| {
        let left = t.replace("{value}", "");
        assert!(!left.contains('{') && !left.contains('}'), "{who}: unknown placeholder in '{t}'");
    };
    let relative = |f: &str| {
        assert!(!f.is_empty() && !f.starts_with('/') && !f.starts_with('\\') && !f.contains(':') && !f.split(['/', '\\']).any(|x| x == ".."), "{who}: '{f}' must be a plain relative path");
    };
    match e {
        Apply::Args { args } => {
            assert!(!args.is_empty(), "{who}: empty argument list");
            args.iter().for_each(|a| placeholders_ok(a));
        }
        Apply::Ini { file, section, sets, .. } => {
            relative(file);
            assert!(!section.is_empty(), "{who}: an INI edit needs a section");
            assert!(!sets.is_empty(), "{who}: an INI edit needs keys");
            sets.iter().for_each(|kv| {
                assert!(!kv.key.is_empty());
                placeholders_ok(&kv.value);
            });
        }
        Apply::Toml { file, path, value, .. } => {
            relative(file);
            assert!(!path.is_empty() && !path.split('.').any(str::is_empty), "{who}: bad TOML path '{path}'");
            placeholders_ok(value);
        }
        Apply::RetroarchOption { sets } => {
            assert!(p.id == "retroarch" && s.core.is_some(), "{who}: libretro options only make sense for a RetroArch core");
            assert!(!sets.is_empty());
            sets.iter().for_each(|kv| placeholders_ok(&kv.value));
        }
    }
}

/// Dolphin skips a `-C` option whose system name it doesn't know, silently. Its graphics system is
/// `Graphics` on the command line even though its file is GFX.ini (Common/Config/Config.cpp).
#[test]
fn dolphin_is_told_its_graphics_setting_by_the_name_it_actually_answers_to() {
    let args = plan_args("dolphin", "resolution", "3");
    assert_eq!(args, ["-C", "Graphics.Settings.InternalResolution=3"]);
    assert!(!args.concat().contains("GFX."), "{args:?}");
}

/// Azahar ignores a value while its `\default` flag is true, so the flag is part of the setting.
#[test]
fn azahar_resolution_sets_the_value_and_clears_its_default_flag() {
    let (_t, tuning) = plan_in_tmp("azahar", "resolution", "3");
    let [FileEdit { op: Op::Ini { section, sets, .. }, .. }] = tuning.edits.as_slice() else { panic!("{:?}", tuning.edits) };
    assert_eq!(section, "Renderer");
    assert_eq!(sets, &[("resolution_factor\\default".to_string(), "false".to_string()), ("resolution_factor".to_string(), "3".to_string())]);
}

// ------------------------------------------------------------------------------------ planning

fn plan_args(id: &str, setting: &str, value: &str) -> Vec<String> {
    plan_in_tmp(id, setting, value).1.args
}

/// Plans one chosen setting for `id` against an empty temp "machine".
fn plan_in_tmp(id: &str, setting: &str, value: &str) -> (tempfile::TempDir, Tuning) {
    let t = tempfile::tempdir().unwrap();
    let (data, config) = (t.path().join("data"), t.path().join("config"));
    let mut values = Values::new();
    values.entry(id.to_string()).or_default().insert(setting.to_string(), value.to_string());
    let tuning = plan(&profile(id), &values, &PlanCtx { data_root: &data, config_root: &config, retroarch: None, core: None });
    (t, tuning)
}

#[test]
fn nothing_is_planned_until_the_user_chooses() {
    let t = tempfile::tempdir().unwrap();
    let cx = PlanCtx { data_root: t.path(), config_root: t.path(), retroarch: None, core: None };
    for p in profiles::builtin() {
        assert!(plan(&p, &Values::new(), &cx).is_empty(), "{} changed something unasked", p.id);
    }
    // An empty value is "don't change", the same as no entry.
    let mut v = Values::new();
    v.entry("pcsx2".into()).or_default().insert("resolution".into(), String::new());
    assert!(plan(&profile("pcsx2"), &v, &cx).is_empty());
}

#[test]
fn a_standalone_emulator_edits_its_settings_file_under_its_data_root() {
    let (t, tuning) = plan_in_tmp("pcsx2", "resolution", "4");
    assert!(tuning.args.is_empty());
    let [e] = tuning.edits.as_slice() else { panic!("{:?}", tuning.edits) };
    assert_eq!(e.path, t.path().join("data/inis/PCSX2.ini"));
    assert!(!e.create && e.seed.is_none(), "never creates an emulator's settings file");
    assert_eq!(e.op, Op::Ini { section: "EmuCore/GS".into(), sets: sets(&[("upscale_multiplier", "4")]), quote: false });
}

#[test]
fn azahar_edits_its_file_under_the_config_root_not_the_data_root() {
    let (t, tuning) = plan_in_tmp("azahar", "resolution", "2");
    assert_eq!(tuning.edits[0].path, t.path().join("config/qt-config.ini"));
}

#[test]
fn a_choice_can_carry_its_own_effect() {
    assert_eq!(plan_args("pcsx2", "fullscreen", "on"), ["-fullscreen"]);
    assert_eq!(plan_args("pcsx2", "fullscreen", "off"), ["-nofullscreen"]);
    let (_t, tuning) = plan_in_tmp("xemu", "fullscreen", "on");
    assert_eq!(tuning.edits[0].op, Op::Toml { path: path("display.window.fullscreen_on_startup"), value: "true".into(), kind: TomlKind::Bool });
}

#[test]
fn a_value_the_profile_no_longer_offers_is_ignored_not_guessed_at() {
    let (_t, tuning) = plan_in_tmp("pcsx2", "resolution", "7.5");
    assert!(tuning.is_empty());
    let (_t, tuning) = plan_in_tmp("pcsx2", "no-such-setting", "1");
    assert!(tuning.is_empty());
}

fn retroarch_root(t: &Path) -> Layout {
    let root = t.join("retroarch");
    std::fs::create_dir_all(root.join("info")).unwrap();
    std::fs::write(root.join("info/flycast_libretro.info"), "corename = \"Flycast\"\n").unwrap();
    std::fs::write(root.join("info/mupen64plus_next_libretro.info"), "corename = \"Mupen64Plus-Next\"\n").unwrap();
    std::fs::write(root.join("retroarch.cfg"), "").unwrap();
    retroarch::layout(&root, crate::paths::Os::Linux, &crate::paths::PathCtx::default())
}

fn retro_values(core: &str, value: &str) -> Values {
    let mut v = Values::new();
    v.entry(format!("retroarch:{core}")).or_default().insert("resolution".into(), value.into());
    v
}

#[test]
fn retroarch_settings_apply_only_to_the_core_that_is_running() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch_root(t.path());
    let values = retro_values("flycast", "1280x960");
    let p = profile("retroarch");
    let cx = |core| PlanCtx { data_root: &layout.root, config_root: &layout.root, retroarch: Some(&layout), core };

    assert!(plan(&p, &values, &cx(Some("mupen64plus_next"))).is_empty(), "a Flycast choice leaked into another core");
    assert!(plan(&p, &values, &cx(None)).is_empty());

    let tuning = plan(&p, &values, &cx(Some("flycast")));
    let [e] = tuning.edits.as_slice() else { panic!("{:?}", tuning.edits) };
    assert_eq!(e.path, layout.root.join("config/Flycast/Flycast.opt"));
    assert_eq!(e.op, Op::Ini { section: String::new(), sets: sets(&[("reicast_internal_resolution", "1280x960")]), quote: true });
    assert!(e.create);
}

#[test]
fn a_core_with_two_options_to_set_sets_both() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch_root(t.path());
    let cx = PlanCtx { data_root: &layout.root, config_root: &layout.root, retroarch: Some(&layout), core: Some("mupen64plus_next") };
    let tuning = plan(&profile("retroarch"), &retro_values("mupen64plus_next", "2"), &cx);
    let [FileEdit { op: Op::Ini { sets: s, .. }, path, .. }] = tuning.edits.as_slice() else { panic!("{:?}", tuning.edits) };
    assert_eq!(s, &sets(&[("mupen64plus-43screensize", "1280x960"), ("mupen64plus-169screensize", "1280x720")]));
    assert_eq!(path, &layout.root.join("config/Mupen64Plus-Next/Mupen64Plus-Next.opt"));
}

#[test]
fn the_per_core_file_follows_retroarchs_own_config_directory_setting() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch_root(t.path());
    std::fs::write(layout.root.join("retroarch.cfg"), format!("rgui_config_directory = \"{}\"\n", t.path().join("elsewhere").display())).unwrap();
    let layout = retroarch::layout(&layout.root, crate::paths::Os::Linux, &crate::paths::PathCtx::default());
    assert_eq!(retroarch::options_target(&layout, "flycast").path, t.path().join("elsewhere/Flycast/Flycast.opt"));
}

#[test]
fn global_core_options_mode_writes_the_global_file_instead() {
    let t = tempfile::tempdir().unwrap();
    let base = retroarch_root(t.path());
    std::fs::write(base.root.join("retroarch.cfg"), "global_core_options = \"true\"\n").unwrap();
    let layout = retroarch::layout(&base.root, crate::paths::Os::Linux, &crate::paths::PathCtx::default());
    let target = retroarch::options_target(&layout, "flycast");
    assert_eq!(target.path, base.root.join("retroarch-core-options.cfg"));
    assert!(target.seed.is_none());
}

// ------------------------------------------------------------------------------------ applying

fn edit(path: &Path, section: &str, key: &str, value: &str) -> FileEdit {
    FileEdit {
        label: "Test · Setting".into(),
        scope: "test:scope".into(),
        path: path.to_path_buf(),
        op: Op::Ini { section: section.into(), sets: sets(&[(key, value)]), quote: false },
        create: false,
        seed: None,
    }
}

fn tuning_of(edits: Vec<FileEdit>) -> Tuning {
    Tuning { edits, ..Default::default() }
}

#[test]
fn applying_changes_the_file_and_keeps_the_original_aside_once() {
    let t = tempfile::tempdir().unwrap();
    let (file, backups) = (t.path().join("PCSX2.ini"), t.path().join("backups"));
    std::fs::write(&file, "[EmuCore/GS]\nupscale_multiplier = 1\nVsync = 0\n").unwrap();

    assert!(tuning_of(vec![edit(&file, "EmuCore/GS", "upscale_multiplier", "3")]).apply(&backups).is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[EmuCore/GS]\nupscale_multiplier = 3\nVsync = 0\n");
    let orig = backups.join("test_scope/PCSX2.ini.orig");
    assert_eq!(std::fs::read_to_string(&orig).unwrap(), "[EmuCore/GS]\nupscale_multiplier = 1\nVsync = 0\n");

    // A later change must not replace the copy of what the emulator itself wrote.
    tuning_of(vec![edit(&file, "EmuCore/GS", "upscale_multiplier", "6")]).apply(&backups);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[EmuCore/GS]\nupscale_multiplier = 6\nVsync = 0\n");
    assert_eq!(std::fs::read_to_string(&orig).unwrap(), "[EmuCore/GS]\nupscale_multiplier = 1\nVsync = 0\n");
}

#[test]
fn a_file_that_already_has_the_value_is_not_rewritten() {
    let t = tempfile::tempdir().unwrap();
    let (file, backups) = (t.path().join("a.ini"), t.path().join("backups"));
    std::fs::write(&file, "[A]\nx = 2\n").unwrap();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(tuning_of(vec![edit(&file, "A", "x", "2")]).apply(&backups).is_empty());
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
    assert!(!backups.exists(), "no backup for a file that wasn't changed");
}

#[test]
fn a_missing_settings_file_is_not_made_up_and_the_user_is_told_what_to_do() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("inis/PCSX2.ini");
    let notes = tuning_of(vec![edit(&file, "EmuCore/GS", "upscale_multiplier", "3")]).apply(&t.path().join("backups"));
    assert!(!file.exists() && !file.parent().unwrap().exists(), "nothing was created");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("Test · Setting") && notes[0].contains("Start the emulator once"), "{notes:?}");
}

#[test]
fn a_file_that_is_not_text_is_left_alone() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("a.ini");
    std::fs::write(&file, [0xff, 0xfe, 0x00, 0x5b]).unwrap();
    let notes = tuning_of(vec![edit(&file, "A", "x", "1")]).apply(&t.path().join("b"));
    assert!(notes[0].contains("isn't UTF-8"), "{notes:?}");
    assert_eq!(std::fs::read(&file).unwrap(), [0xff, 0xfe, 0x00, 0x5b]);
}

#[test]
fn a_toml_file_that_does_not_parse_is_left_alone() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("xemu.toml");
    std::fs::write(&file, "[display\nbroken").unwrap();
    let e = FileEdit {
        op: Op::Toml { path: path("display.quality.surface_scale"), value: "2".into(), kind: TomlKind::Int },
        ..edit(&file, "", "", "")
    };
    let notes = tuning_of(vec![e]).apply(&t.path().join("b"));
    assert!(notes[0].contains("valid TOML"), "{notes:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[display\nbroken");
}

#[test]
fn a_retroarch_core_options_file_is_created_seeded_from_the_global_one() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch_root(t.path());
    std::fs::write(&layout.global_options, "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"640x480\"\n").unwrap();
    let cx = PlanCtx { data_root: &layout.root, config_root: &layout.root, retroarch: Some(&layout), core: Some("flycast") };
    let tuning = plan(&profile("retroarch"), &retro_values("flycast", "1920x1440"), &cx);

    assert!(tuning.apply(&t.path().join("backups")).is_empty());
    let per_core = layout.root.join("config/Flycast/Flycast.opt");
    // The user's other option for the core carries over; without the seed RetroArch would stop reading it.
    assert_eq!(std::fs::read_to_string(&per_core).unwrap(), "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"1920x1440\"\n");
    assert_eq!(std::fs::read_to_string(&layout.global_options).unwrap(), "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"640x480\"\n", "the global file isn't touched");
    assert!(!t.path().join("backups").exists(), "a file we created has no original to keep");
}

#[test]
fn an_existing_per_core_file_is_edited_in_place_not_reseeded() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch_root(t.path());
    std::fs::write(&layout.global_options, "reicast_cable_type = \"RGB\"\n").unwrap();
    let per_core = layout.root.join("config/Flycast/Flycast.opt");
    std::fs::create_dir_all(per_core.parent().unwrap()).unwrap();
    std::fs::write(&per_core, "reicast_cable_type = \"VGA\"\n").unwrap();
    let cx = PlanCtx { data_root: &layout.root, config_root: &layout.root, retroarch: Some(&layout), core: Some("flycast") };
    plan(&profile("retroarch"), &retro_values("flycast", "1280x960"), &cx).apply(&t.path().join("backups"));
    assert_eq!(std::fs::read_to_string(&per_core).unwrap(), "reicast_cable_type = \"VGA\"\nreicast_internal_resolution = \"1280x960\"\n");
}

#[test]
fn the_user_hears_when_a_per_game_options_file_will_outrank_the_setting() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch_root(t.path());
    let dir = layout.root.join("config/Flycast");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Crazy Taxi.opt"), "").unwrap();
    std::fs::write(dir.join("Flycast.opt"), "").unwrap(); // the core's own file is not an override
    let cx = PlanCtx { data_root: &layout.root, config_root: &layout.root, retroarch: Some(&layout), core: Some("flycast") };
    let notes = plan(&profile("retroarch"), &retro_values("flycast", "1280x960"), &cx).apply(&t.path().join("b"));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("1 per-game or per-folder option file") && notes[0].contains("take priority"), "{notes:?}");
}

#[test]
fn retroarch_that_has_never_run_is_a_message_not_a_made_up_folder() {
    let t = tempfile::tempdir().unwrap();
    let layout = retroarch::layout(&t.path().join("never-run"), crate::paths::Os::Linux, &crate::paths::PathCtx::default());
    let cx = PlanCtx { data_root: &layout.root, config_root: &layout.root, retroarch: Some(&layout), core: Some("flycast") };
    let tuning = plan(&profile("retroarch"), &retro_values("flycast", "1280x960"), &cx);
    assert!(tuning.edits.is_empty());
    assert!(tuning.notes[0].contains("Start RetroArch once"), "{:?}", tuning.notes);
    assert!(!layout.root.exists());
}

// ------------------------------------------------------------------------------------ the stored values

#[test]
fn sanitize_forgets_what_the_catalogue_no_longer_has() {
    let mut v = Values::new();
    v.entry("pcsx2".into()).or_default().extend([("resolution".to_string(), "4".to_string()), ("gone".to_string(), "1".to_string()), ("fullscreen".to_string(), "sideways".to_string())]);
    v.entry("retired-emulator".into()).or_default().insert("resolution".into(), "2".into());
    v.entry("dolphin".into()).or_default().insert("resolution".into(), String::new());
    v.entry("retroarch:flycast".into()).or_default().insert("resolution".into(), "1280x960".into());

    let clean = sanitize(v);
    assert_eq!(clean.keys().map(String::as_str).collect::<Vec<_>>(), ["pcsx2", "retroarch:flycast"]);
    assert_eq!(clean["pcsx2"], BTreeMap::from([("resolution".to_string(), "4".to_string())]));
}

#[test]
fn the_schema_lists_retroarch_one_core_at_a_time() {
    let all = scopes();
    let ids: Vec<&str> = all.iter().map(|s| s.scope.as_str()).collect();
    for want in ["dolphin", "pcsx2", "duckstation", "ppsspp", "azahar", "xemu", "retroarch:flycast", "retroarch:mupen64plus_next", "retroarch:swanstation"] {
        assert!(ids.contains(&want), "{want} missing from {ids:?}");
    }
    assert!(!ids.contains(&"retroarch"), "RetroArch has no settings of its own, only its cores do");
    let fly = all.iter().find(|s| s.scope == "retroarch:flycast").unwrap();
    assert_eq!(fly.name, "RetroArch · Flycast");
    assert_eq!(fly.core.as_deref(), Some("flycast"));
}

#[test]
fn a_scope_says_whether_it_edits_files_or_only_uses_the_command_line() {
    let all = scopes();
    let by = |id: &str| all.iter().find(|s| s.scope == id).unwrap().changes_files();
    assert!(!by("dolphin"), "Dolphin is controlled by arguments alone");
    for id in ["pcsx2", "duckstation", "ppsspp", "azahar", "xemu", "retroarch:flycast"] {
        assert!(by(id), "{id} edits a file");
    }
}
