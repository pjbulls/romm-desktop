//! Building the emulator command line.

use std::borrow::Cow;
use std::path::Path;

use tokio::process::Command;

use crate::detect::{InstallKind, Installed};
use crate::error::{Error, Result};
use crate::profiles::Profile;
use crate::unit;

/// What to hand the emulator for an installed game. Normally the stored launch file, as it is. A
/// multi-disc game is installed with its `.m3u` playlist as that file, which an emulator that can't
/// open playlists (`launch.no_playlists`) would read as a disc image and refuse; it gets the first
/// disc the playlist lists instead. Done here, when launching, rather than once at install, so a
/// game that is already installed is repaired too.
///
/// Xenia asks for the other discs itself, with a file picker, when the game reaches them.
pub fn playable<'a>(profile: &Profile, rom: &'a Path) -> Result<Cow<'a, Path>> {
    if !profile.launch.no_playlists || !unit::is_playlist(rom) {
        return Ok(Cow::Borrowed(rom));
    }
    let disc = unit::resolve_playlist(rom);
    if disc == rom {
        return Err(Error::msg(format!(
            "{} can't open playlists, and {} doesn't name a first disc that exists next to it. \
             Re-install the game, or edit the playlist so its first line is the first disc's file name",
            profile.name,
            rom.display()
        )));
    }
    Ok(Cow::Owned(disc))
}

pub fn build_command(
    profile: &Profile,
    installed: &Installed,
    rom: &Path,
    core: Option<&Path>,
    extra_args: &[String],
) -> Result<Command> {
    let rom = playable(profile, rom)?;
    let mut args: Vec<String> = extra_args.to_vec();
    for a in &profile.launch.args {
        args.push(match a.as_str() {
            "{rom}" => rom.to_string_lossy().into_owned(),
            "{core}" => core
                .ok_or_else(|| Error::msg(format!("{} needs a libretro core for this platform", profile.name)))?
                .to_string_lossy()
                .into_owned(),
            other => other.to_string(),
        });
    }
    let mut cmd = match &installed.kind {
        InstallKind::Native(exe) => Command::new(exe),
        InstallKind::Flatpak(id) => {
            let mut c = Command::new("flatpak");
            c.arg("run").arg(id);
            c
        }
    };
    cmd.args(args);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles;

    fn args_of(c: &Command) -> Vec<String> {
        c.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn dolphin_native_and_flatpak() {
        let p = profiles::get("dolphin").unwrap();
        let native = Installed { profile_id: "dolphin".into(), kind: InstallKind::Native("/usr/bin/dolphin-emu".into()) };
        let c = build_command(&p, &native, Path::new("/g/Melee.iso"), None, &[]).unwrap();
        assert_eq!(c.as_std().get_program(), "/usr/bin/dolphin-emu");
        assert_eq!(args_of(&c), ["-b", "-e", "/g/Melee.iso"]);

        let fp = Installed { profile_id: "dolphin".into(), kind: InstallKind::Flatpak("org.DolphinEmu.dolphin-emu".into()) };
        let c = build_command(&p, &fp, Path::new("/g/Melee.iso"), None, &[]).unwrap();
        assert_eq!(c.as_std().get_program(), "flatpak");
        assert_eq!(args_of(&c), ["run", "org.DolphinEmu.dolphin-emu", "-b", "-e", "/g/Melee.iso"]);
    }

    #[test]
    fn retroarch_needs_core_and_extra_args_come_first() {
        let p = profiles::get("retroarch").unwrap();
        let i = Installed { profile_id: "retroarch".into(), kind: InstallKind::Native("/usr/bin/retroarch".into()) };
        assert!(build_command(&p, &i, Path::new("/g/z.sfc"), None, &[]).is_err());
        let c = build_command(&p, &i, Path::new("/g/z.sfc"), Some(Path::new("/c/snes9x_libretro.so")), &["-f".into()]).unwrap();
        assert_eq!(args_of(&c), ["-f", "-L", "/c/snes9x_libretro.so", "/g/z.sfc"]);
    }

    #[test]
    fn azahar_game_goes_last_so_extra_flags_still_work() {
        let p = profiles::get("azahar").unwrap();
        let native = Installed { profile_id: "azahar".into(), kind: InstallKind::Native("/usr/bin/azahar".into()) };
        let rom = Path::new("/g/Super Mario 3D Land.cci");
        let c = build_command(&p, &native, rom, None, &[]).unwrap();
        assert_eq!(c.as_std().get_program(), "/usr/bin/azahar");
        assert_eq!(args_of(&c), ["/g/Super Mario 3D Land.cci"]);
        // Azahar only takes the game from the last argument when other flags are present.
        let c = build_command(&p, &native, rom, None, &["-f".into()]).unwrap();
        assert_eq!(args_of(&c), ["-f", "/g/Super Mario 3D Land.cci"]);

        let fp = Installed { profile_id: "azahar".into(), kind: InstallKind::Flatpak("org.azahar_emu.Azahar".into()) };
        let c = build_command(&p, &fp, rom, None, &[]).unwrap();
        assert_eq!(c.as_std().get_program(), "flatpak");
        assert_eq!(args_of(&c), ["run", "org.azahar_emu.Azahar", "/g/Super Mario 3D Land.cci"]);
    }

    #[test]
    fn xemu_loads_the_disc_with_dvd_path() {
        let p = profiles::get("xemu").unwrap();
        let native = Installed { profile_id: "xemu".into(), kind: InstallKind::Native("/usr/bin/xemu".into()) };
        let rom = Path::new("/g/Halo - Combat Evolved.iso");
        let c = build_command(&p, &native, rom, None, &["-full-screen".into()]).unwrap();
        assert_eq!(args_of(&c), ["-full-screen", "-dvd_path", "/g/Halo - Combat Evolved.iso"]);

        let fp = Installed { profile_id: "xemu".into(), kind: InstallKind::Flatpak("app.xemu.xemu".into()) };
        let c = build_command(&p, &fp, rom, None, &[]).unwrap();
        assert_eq!(args_of(&c), ["run", "app.xemu.xemu", "-dvd_path", "/g/Halo - Combat Evolved.iso"]);
    }

    #[test]
    fn duckstation_stops_option_parsing_before_path() {
        let p = profiles::get("duckstation").unwrap();
        let i = Installed { profile_id: "duckstation".into(), kind: InstallKind::Native("/x/duckstation-qt".into()) };
        let c = build_command(&p, &i, Path::new("/g/-weird name.cue"), None, &[]).unwrap();
        assert_eq!(args_of(&c), ["-batch", "--", "/g/-weird name.cue"]);
    }

    /// Extra flags come first and `--` guards the game, so a ROM named like a flag is still a game.
    #[test]
    fn xenia_takes_the_game_as_the_positional_target_after_double_dash() {
        let p = profiles::get("xenia").unwrap();
        let native = Installed { profile_id: "xenia".into(), kind: InstallKind::Native("/opt/xenia/xenia_canary".into()) };
        let c = build_command(&p, &native, Path::new("/g/Halo 3.iso"), None, &[]).unwrap();
        assert_eq!(c.as_std().get_program(), "/opt/xenia/xenia_canary");
        assert_eq!(args_of(&c), ["--", "/g/Halo 3.iso"]);
        let c = build_command(&p, &native, Path::new("/g/-odd.iso"), None, &["--fullscreen".into()]).unwrap();
        assert_eq!(args_of(&c), ["--fullscreen", "--", "/g/-odd.iso"]);
    }

    fn game(files: &[(&str, &str)]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for (f, body) in files {
            std::fs::write(t.path().join(f), body).unwrap();
        }
        t
    }

    fn xenia() -> (Profile, Installed) {
        (profiles::get("xenia").unwrap(), Installed { profile_id: "xenia".into(), kind: InstallKind::Native("/x/xenia_canary".into()) })
    }

    /// The reported failure: a multi-disc game's playlist reached Xenia, which read it as a disc image.
    #[test]
    fn xenia_is_given_the_first_disc_not_the_playlist() {
        let t = game(&[("MyGame (Disk 1).iso", "1"), ("MyGame (Disk 2).iso", "2"), ("MyGame.m3u", "MyGame (Disk 1).iso\nMyGame (Disk 2).iso\n")]);
        let (p, i) = xenia();
        let c = build_command(&p, &i, &t.path().join("MyGame.m3u"), None, &["--fullscreen".into()]).unwrap();
        let disc = t.path().join("MyGame (Disk 1).iso");
        assert_eq!(args_of(&c), ["--fullscreen", "--", disc.to_str().unwrap()]);
    }

    #[test]
    fn xenia_is_given_a_plain_disc_exactly_as_before() {
        let (p, i) = xenia();
        let c = build_command(&p, &i, Path::new("/g/Halo 3.iso"), None, &[]).unwrap();
        assert_eq!(args_of(&c), ["--", "/g/Halo 3.iso"]);
    }

    #[test]
    fn a_playlist_with_no_first_disc_to_be_found_is_an_error_that_says_what_to_do() {
        let t = game(&[("MyGame.m3u", "MyGame (Disk 1).iso\n")]); // the disc is not there
        let (p, i) = xenia();
        let e = build_command(&p, &i, &t.path().join("MyGame.m3u"), None, &[]).unwrap_err().to_string();
        assert!(e.contains("can't open playlists") && e.contains("MyGame.m3u") && e.contains("first disc"), "{e}");
    }

    /// Only emulators that say they can't read playlists are changed; the rest still get the playlist.
    #[test]
    fn emulators_that_read_playlists_still_get_the_playlist() {
        let t = game(&[("Game (Disc 1).chd", "1"), ("Game.m3u", "Game (Disc 1).chd\n")]);
        let m3u = t.path().join("Game.m3u");
        let ds = profiles::get("duckstation").unwrap();
        let ds_i = Installed { profile_id: "duckstation".into(), kind: InstallKind::Native("/x/duckstation".into()) };
        assert_eq!(args_of(&build_command(&ds, &ds_i, &m3u, None, &[]).unwrap()).last().unwrap(), m3u.to_str().unwrap());
        let ra = profiles::get("retroarch").unwrap();
        let ra_i = Installed { profile_id: "retroarch".into(), kind: InstallKind::Native("/x/retroarch".into()) };
        let c = build_command(&ra, &ra_i, &m3u, Some(Path::new("/c/core.so")), &[]).unwrap();
        assert!(args_of(&c).contains(&m3u.to_string_lossy().into_owned()));
    }
}
