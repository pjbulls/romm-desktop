//! Turning config + a platform into "this emulator, installed here, data lives there, run this command".
//! Shared by the CLI and the desktop app.

use std::path::PathBuf;

use tokio::process::Command;

use crate::config::Config;
use crate::detect::{self, Installed};
use crate::error::{Error, Result};
use crate::launch;
use crate::paths::{resolve_data_root, Os, PathCtx};
use crate::profiles::{self, Profile};
use crate::retroarch;
use crate::state::LocalRom;
use crate::sync::EmuEnv;
use crate::platforms;

pub struct Resolver<'a> {
    pub cfg: &'a Config,
    pub os: Os,
    pub pctx: PathCtx,
}

pub struct Prepared {
    pub env: EmuEnv,
    pub installed: Installed,
    pub command: Command,
}

impl<'a> Resolver<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        Resolver { cfg, os: Os::current(), pctx: PathCtx::from_env() }
    }

    pub fn installed(&self, profile: &Profile) -> Option<Installed> {
        let path_env = std::env::var_os("PATH");
        detect::detect(
            profile,
            self.cfg.emulator_paths.get(&profile.id).map(PathBuf::as_path),
            self.os,
            &self.pctx,
            path_env.as_deref(),
        )
    }

    /// Which emulator profile plays this RomM platform.
    pub fn emulator_for(&self, slug: &str) -> Result<String> {
        platforms::emulator_for(slug, &self.cfg.platform_emulators)
            .ok_or_else(|| Error::msg(format!("no emulator is configured for platform '{slug}'")))
    }

    /// Profile + where it's installed + where its data lives.
    pub fn emu_env(&self, emu_id: &str) -> Result<(EmuEnv, Installed)> {
        let profile = profiles::get(emu_id).ok_or_else(|| Error::msg(format!("unknown emulator '{emu_id}'")))?;
        let installed = self.installed(&profile).ok_or_else(|| {
            Error::msg(format!(
                "{} was not found. Install it, or choose its executable under Emulators (CLI: `romm emulator-set {} <path>`)",
                profile.name, profile.id
            ))
        })?;
        let pctx = self.pctx.clone().with_exe_dir(installed.exe_dir());
        let root = resolve_data_root(&profile, installed.is_flatpak(), self.os, &pctx)
            .ok_or_else(|| Error::msg(format!("could not work out where {} keeps its data", profile.name)))?;
        let retro = (profile.id == "retroarch").then(|| retroarch::layout(&root.path, self.os, &pctx));
        Ok((EmuEnv { profile, data_root: root.path, retroarch: retro }, installed))
    }

    /// Everything needed to launch an installed game.
    pub fn prepare(&self, rom: &LocalRom) -> Result<Prepared> {
        let emu_id = self.emulator_for(&rom.platform_slug)?;
        let (env, installed) = self.emu_env(&emu_id)?;
        let core = match &env.retroarch {
            Some(layout) => {
                let name = self
                    .cfg
                    .retroarch_cores
                    .get(&rom.platform_slug)
                    .map(String::as_str)
                    .or_else(|| retroarch::default_core(&rom.platform_slug))
                    .ok_or_else(|| Error::msg(format!("no RetroArch core chosen for '{}'", rom.platform_slug)))?;
                Some(retroarch::find_core(layout, name, self.os).ok_or_else(|| {
                    Error::msg(format!("RetroArch core '{name}' isn't installed (in RetroArch: Load Core → Download a Core)"))
                })?)
            }
            None => None,
        };
        let extra = self.cfg.extra_args.get(&emu_id).cloned().unwrap_or_default();
        let command = launch::build_command(&env.profile, &installed, &rom.launch_path, core.as_deref(), &extra)?;
        Ok(Prepared { env, installed, command })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_builds_a_command_for_an_overridden_emulator() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("PPSSPPSDL");
        std::fs::write(&exe, "").unwrap();
        let mut cfg = Config::default();
        cfg.emulator_paths.insert("ppsspp".into(), exe.clone());
        let rom = LocalRom {
            rom_id: 1, name: "Daxter".into(), platform_slug: "psp".into(), path: "/r".into(),
            launch_path: "/r/daxter.iso".into(), stem: "daxter".into(), ..Default::default()
        };
        let r = Resolver { cfg: &cfg, os: Os::Linux, pctx: PathCtx { home: Some(t.path().into()), xdg_config: Some(t.path().join("cfg")), ..Default::default() } };
        let p = r.prepare(&rom).unwrap();
        assert_eq!(p.command.as_std().get_program(), exe.as_os_str());
        assert!(p.env.data_root.ends_with("cfg/ppsspp/PSP"), "{:?}", p.env.data_root);
    }

    #[test]
    fn missing_emulator_message_tells_the_user_what_to_do() {
        let cfg = Config::default();
        let r = Resolver { cfg: &cfg, os: Os::Linux, pctx: PathCtx::default() };
        let msg = r.emu_env("pcsx2").err().unwrap().to_string();
        assert!(msg.contains("PCSX2 was not found") && msg.contains("Emulators"), "{msg}");
    }
}
