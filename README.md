# flora

Wii U emulator written in Rust, structured after the hardware and system
software of the console. Its design references [Cemu](https://github.com/cemu-project/Cemu).

```text
cargo xtask run [--graphics wgpu|vulkan|opengl] [-- TITLE_DIR | FILE.rpx]
cargo xtask test
```

- `wgpu` picks the best backend (DX12 / Metal / Vulkan / GL), `vulkan` forces
  wgpu's Vulkan backend, `opengl` uses native OpenGL through glow.
- Settings are stored in `flora.toml` in the working directory.
- ROM directories are listed in `roms.json` (git-ignored; copy
  `roms.example.json`). Each directory may hold extracted titles
  (`<name>/code`, `content`, `meta`) and `.rpx` files; they appear in the
  "Games" panel, double-click to boot.
- Headless boot for debugging:
  `cargo run --release -p flora_tests --example headless -- <TITLE_DIR> [FRAMES]`
- Default keys: A=`X` B=`Z` X=`S` Y=`A` L=`Q` R=`W` ZL=`1` ZR=`2`
  +=`Enter` −=`Backspace` D-pad=arrows, left stick=`IJKL`.

The project is early: homebrew that clears and flips the screens runs; 3D
rendering, threads and audio output are not implemented yet. See
[docs/ref-wiiubrew](docs/ref-wiiubrew/01_emulator_architecture.md) for the
architecture and status.

## License

MPL-2.0
