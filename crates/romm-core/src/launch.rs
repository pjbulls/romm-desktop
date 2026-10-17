//! Building the emulator command line.

use std::path::Path;

use tokio::process::Command;

use crate::detect::{InstallKind, Installed};
use crate::error::{Error, Result};
use crate::profiles::Profile;

pub fn build_command(
    profile: &Profile,
    installed: &Installed,
    rom: &Path,
    core: Option<&Path>,
    extra_args: &[String],
) -> Result<Command> {
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
}
