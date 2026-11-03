# shocora

<p align="center">
  <img src="gui/egui/assets/shocora.png" alt="shocora" width="150"/>
</p>

Wii U emulator written in Rust, structured after the hardware and system
software of the console. Its design references [Cemu](https://github.com/cemu-project/Cemu).

## Images

<p align="center">
  <img src="./docs/img/launcher.png" alt="launcher" width="500"/>
</p>
<p align="center">
  <img src="./docs/img/zelda_tw.png" alt="zelda" width="500"/>
</p>
<p align="center">
  <img src="./docs/img/mario_tennis.png" alt="mario 0" width="500"/>
</p>

## Quick Start

```text
cargo xtask run [--graphics wgpu|vulkan|opengl] [-- TITLE_DIR | FILE.rpx]
cargo xtask test
cargo xtask icon   # gui/egui/assets/shocora.svg -> shocora.png, shocora.ico
```

## Implimentation

- `wgpu` picks the best backend (DX12 / Metal / Vulkan / GL), `vulkan` forces
  wgpu's Vulkan backend, `opengl` uses native OpenGL through glow.
- Settings are stored in `shocora.toml` in the working directory.
- ROM directories are listed in `roms.json` (git-ignored; copy
  `roms.example.json`). Each directory may hold extracted titles
  (`<name>/code`, `content`, `meta`) and `.rpx` files; they appear in the
  "Games" panel, double-click to boot.
- Headless boot for debugging:
  `cargo run --release -p shocora_tests --example headless -- <TITLE_DIR> [FRAMES]`
- Default keys: A=`X` B=`Z` X=`S` Y=`A` L=`Q` R=`W` ZL=`1` ZR=`2`
  +=`Enter` −=`Backspace` D-pad=arrows, left stick=`IJKL`.
- Controllers work through [gilrs](https://gitlab.com/gilrs-project/gilrs) as soon as they
  are plugged in; buttons go by position (`East` is the Wii U's A). `[pad_bindings]` in
  `shocora.toml` changes the mapping.
- Settings has a page each for general, graphics, input and netplay settings.
- Settings > General changes the language. The texts are in `lang/translation.eng.json` and
  `lang/translation.jpn.json`, built into the program; a `lang` folder in the working
  directory takes precedence, so they can be edited without rebuilding.
- Settings > Graphics > 3D resolution draws the title's pictures 2 to 8 times larger, up to
  10240 pixels wide (8x at 720p, 5x at 1080p). This takes a lot of video memory.
- Settings > Graphics > Screen upscaling runs xBRZ on the GPU at 2x to 6x. It is GPL-3.0 code behind
  the default `xbrz` feature of `shocora_render`.
- Settings > Graphics > Frame rate > Force 60 fps replaces the title's swap interval. Titles made for
  30 fps whose logic counts frames, such as Twilight Princess HD, then run at up to
  double speed; the default keeps the frame rate the title asks for. Max 30 fps caps
  titles that run too fast and leaves slower ones alone.
- Tab, pressing the left stick, or the status bar's speed cycles the game speed:
  1x, 2x, 3x, 4x, 0.5x. Sound follows like a tape, so its pitch changes with the speed. The
  left stick's press then does not reach the title.
- Netplay lets a second player join over the local network. The host enters this PC's IP
  address (`192.168.1.10`, or `10.1.2.3/16` for a wider network) and starts hosting on port
  47800; only guests from that network get in, and a password is optional. The guest enters
  the host's address and plays as a Pro Controller, seeing the TV at 640 x 360, 20 fps and
  hearing 16 kHz sound. Each side pings once a second; 3 s of silence ends the link and the
  guest keeps trying to reconnect. The password itself is never sent, but nothing is
  encrypted: use it on networks you trust.
- The driver's compiled shaders are kept in `shader_cache/`, so later runs stutter less.

## References

- [docs/ref-wiiubrew](docs/ref-wiiubrew/01_emulator_architecture.md)
- [Wii U Architecture](https://www.copetti.org/writings/consoles/wiiu/)
- [Wii U Home Brew](https://wiiubrew.org/wiki/Main_Page)
- [Cemu](https://github.com/cemu-project/Cemu): Wii U Emulator written in  C++

## License

SPDX-FileCopyrightText: (C) 2026 Cemu Project
SPDX-License-Identifier: MPL-2.0
