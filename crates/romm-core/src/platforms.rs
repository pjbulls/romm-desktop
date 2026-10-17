//! Which emulator plays which RomM platform.

use std::collections::BTreeMap;

use crate::profiles;
use crate::retroarch;

/// Resolution order: user override → a profile that claims the platform → RetroArch
/// if it has a default core for it.
pub fn emulator_for(slug: &str, overrides: &BTreeMap<String, String>) -> Option<String> {
    if let Some(id) = overrides.get(slug) {
        return Some(id.clone());
    }
    if let Some(p) = profiles::builtin().into_iter().find(|p| p.platforms.iter().any(|s| s == slug)) {
        return Some(p.id);
    }
    retroarch::default_core(slug).map(|_| "retroarch".to_string())
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
    fn override_wins() {
        let mut o = BTreeMap::new();
        o.insert("psx".to_string(), "retroarch".to_string());
        assert_eq!(emulator_for("psx", &o).unwrap(), "retroarch");
    }
}
