# 2. Workspace Layout

Rule: **`core/*` knows nothing about windows, file pickers, graphics APIs or
audio devices.** Host integration lives in `gui/*`.

```text
gui/egui (flora_egui) ─► gui/common (flora_gui_common) ─► gui/render (flora_render)
                                  │
                                  ▼
                        core/system (wiiu_emu)
   ┌─────────┬──────────┬──────┬──┴────────┬─────────────┐
 cafe_os ─► iosu ─► fs   rpl   title   peripherals   sound
   │  └──────────────────▲
   ├─► gpu ─┐            │
   └─► cpu  ├─► memory ─► mem_addr
            └─► interrupt
```

| Crate              | Path               | Role                                                        |
| ------------------ | ------------------ | ----------------------------------------------------------- |
| `wiiu_mem_addr`    | core/mem_addr      | Address map, effective ↔ physical translation               |
| `wiiu_interrupt`   | core/interrupt     | Interrupt sources and pending sets                          |
| `wiiu_memory`      | core/memory        | Mapped regions, big-endian access, `GuestPtr<T>`            |
| `wiiu_cpu`         | core/cpu           | Espresso: decode, interpreter, disassembler (`Bus` trait)   |
| `wiiu_gpu`         | core/gpu           | Latte: PM4, registers, surfaces, display, `RenderCommand`   |
| `wiiu_sound`       | core/sound         | AX-style voices, DSP-ADPCM, mixer                           |
| `wiiu_peripherals` | core/peripherals   | Timebase, RTC/calendar, GamePad state                       |
| `wiiu_title`       | core/title         | Title dirs, meta/app/cos XML, WUX images                    |
| `wiiu_fs`          | core/fs            | Mount table over host directories                           |
| `wiiu_rpl`         | core/rpl           | RPX/RPL parsing, placement, relocation, import resolution   |
| `wiiu_iosu`        | core/iosu          | IOSU devices (`/dev/fsa`, …)                                |
| `wiiu_cafe_os`     | core/cafe_os       | HLE libraries, registry/stubs, process boot                 |
| `wiiu_hle_macros`  | core/hle_macros    | `#[hle_export]`                                             |
| `wiiu_emu`         | core/system        | `Emulator`: wiring, frame loop, title loading               |
| `flora_render`     | gui/render         | `WgpuRenderer`, `GlowRenderer`, `GraphicsApi`               |
| `flora_gui_common` | gui/common         | Emulation thread, config, key bindings                      |
| `flora_egui`       | gui/egui           | The application                                             |
| `xtask`            | xtask              | `cargo xtask build/run/test`                                |

## 2.1 Writing an HLE function

```rust
#[hle_export(lib = "coreinit", name = "OSGetCoreId")]
fn os_get_core_id(ctx: &mut HleContext<'_>) -> u32 {
    ctx.cpu.core_id()
}
```

Arguments after `ctx` are read from r3…/f1… (`FromArg`), the return value is
written back (`IntoReturn`), and `Result<T, HleError>` stops emulation on
error. Add the generated constant (`OS_GET_CORE_ID`) to the library's
`EXPORTS` table. Variadic functions take a trailing `VaStart`.

## 2.2 Building

```text
cargo xtask run -- path/to/title          # default graphics API from flora.toml
cargo xtask run --graphics vulkan -- game.rpx
cargo xtask run --graphics opengl
cargo xtask test
```

On Windows without the MSVC build tools, use the GNU toolchain:
`cargo +stable-x86_64-pc-windows-gnu xtask run`.
