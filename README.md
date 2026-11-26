# RomM Desktop

A Windows + Linux desktop client for [RomM](https://github.com/rommapp/romm), modelled on [Argosy](https://github.com/rommapp/argosy-launcher) for Android. All vibes ⭐🎉🚀

## Features

- **Library**: sync your RomM platforms, cover art, save games, and save states.
- **Play**: download and install ROMs locally, pull saves, launch emulators, and push changes back to the server.
- **Emulators**: auto-detect or manually locate installed executables (n.b. this app does *not* manage installs directly), move BIOS files into known folders.   
Known emulators include:
  + [RetroArch](https://www.retroarch.com/) (200+ cores)
  + [DuckStation](https://duckstation.org/) (Sony PS1)
  + [PPSSPP](https://www.ppsspp.org/) (Sony PSP)
  + [PCSX2](https://pcsx2.net/) (Sony PS2)
  + [Dolphin](https://dolphin-emu.org/) (Nintendo GameCube & Wii)
  + [Azahar](https://azahar-emu.org/) (Nintendo 3DS)
  + [xemu](https://xemu.app/) (Microsoft Xbox)
  + [Xenia Canary](https://github.com/xenia-canary/xenia-canary) (Microsoft Xbox 360)
  + More can be configured via additional [profiles](./crates/romm-core/src/profiles)
- **Controller support**: browse, play, and configure with no mouse or keyboard (excluding file picker dialogs).
- **Interoperable**: bidirectional save file compatibility with [Argosy](https://github.com/rommapp/argosy-launcher).

##  Setup

### Server
You'll need a RomM client API token with these scopes:

`me.read platforms.read roms.read firmware.read assets.read assets.write devices.read devices.write`

If desired, `me.write` is also needed to request a [RetroAchievements](https://retroachievements.org/) sync after a game is closed (see next).
The server connection is managed under **Settings** → **Connection**.

### Emulators
Emulators must be installed separately and can be auto-detected or manually located under **Emulators & BIOS** → **Executables**. This pane
also lets you copy BIOS files from the server, map platforms to emulators (or hide them from the UI), and set a few emulator settings directly.
Most emulator settings should be set in their respective applications though.

**This includes your RetroAchievements account**: for those the app itself only displays what your RomM server knows & does not directly connect to retroachievements.org.

## Build
Requires Rust 1.91+, Node 22.12+, and a C compiler for [`argosy-sigil`](https://github.com/rommapp/argosy-sigil).

```sh
sudo apt-get install build-essentials pkg-config \
  libgtk-3-dev libwebkit2gtk-4.1-dev libdbus-1-dev

cd app
npm ci
npm run tauri build -- --bundles deb
```

If you want to build the standalone CLI as well:

```sh
npm run build:all
```

Artifacts will appear under `target/release`. Runtime dependencies for Debian/Ubuntu:

```sh
sudo apt-get install libwebkit2gtk-4.1-0 libgtk-3-0t64 libsoup-3.0-0 libdbus-1-3
```

License: GPL-3.0-or-later
