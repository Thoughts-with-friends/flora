# 1. Emulator Architecture

## 1.1 What a Wii U is

```text
┌──────────────────────── Wii U ─────────────────────────┐
│  Espresso (3 × PowerPC 750CL @ 1.24 GHz)                │
│    └ runs the game + Cafe OS libraries (coreinit, gx2…) │
│  Latte (R7xx GPU + display controller + Starbuck ARM)   │
│    ├ GPU: PM4 command processor, R600-family shaders     │
│    └ Starbuck runs IOSU (file system, accounts, USB…)    │
│  MEM1 32 MiB eDRAM · MEM2 2 GiB DDR3                     │
│  TV out + GamePad (DRC) stream                           │
└─────────────────────────────────────────────────────────┘
```

## 1.2 Strategy: high-level emulation of the OS

Like Cemu, shocora does not boot the Wii U firmware. Only the game's own
PowerPC code is executed; every system library it imports (`coreinit.rpl`,
`gx2.rpl`, …) and IOSU itself are reimplemented in Rust.

```text
 game code (PowerPC) ──bl OSReport──► HLE stub: [hle_call N; blr]
                                         │
                       StepEvent::HleCall(N) surfaces from wiiu_cpu
                                         │
            wiiu_emu dispatches to wiiu_cafe_os::coreinit::report::os_report
```

The stub opcode (primary opcode 1, unused on Espresso) is the *only* thing the
CPU crate knows about HLE.

## 1.3 Boot

1. `wiiu_title::TitleSource::open` recognises a title directory or `.rpx`.
2. `wiiu_emu` mounts `/vol/code`, `/vol/content`, `/vol/meta`, `/vol/save`.
3. `wiiu_cafe_os::CafeOs::boot` loads the RPX with `wiiu_rpl`
   (segments → TEXT/MEM2/LOADER, imports → HLE stubs), creates the default
   heap and the main stack.
4. Core 1 starts at the entry point with `lr` pointing at an internal stub,
   so returning from the entry point ends the process.

## 1.4 The frame loop

`Emulator::run_frame` runs core 1 until the guest waits for vsync
(`GX2WaitForVsync`, `GX2SwapScanBuffers`), yields, exits, crashes or uses the
per-frame instruction budget. The front end calls it ~60 times per second on
a dedicated thread (`shocora_gui_common::console`).

## 1.5 Graphics path

```text
GX2ClearColor / GX2CopyColorBufferToScanBuffer / GX2SwapScanBuffers
   └─► PM4 packets (incl. emulator-private HLE opcodes 0xE0..)
         └─► wiiu_gpu::command::executor ──► Display ──► RenderCommand
               └─► shocora_render: WgpuRenderer (wgpu: DX12/Metal/Vulkan/GL)
                                 GlowRenderer (native OpenGL)
```

`wiiu_gpu` is API-neutral; renderers live in the front end.

## 1.6 Status

| Area                         | State                                            |
| ---------------------------- | ------------------------------------------------ |
| Espresso interpreter         | integer, branch, load/store, FP, paired singles  |
| RPX/RPL loader               | zlib sections, relocations, trampolines, imports |
| coreinit                     | printf, time, exp/frame heaps, alarms, settings  |
| Threads / scheduler          | cooperative, restartable waits, time slices      |
| Sync                         | mutex, cond, event, semaphore, rendezvous, queue |
| Guest callbacks              | alarm handlers, FS async completion              |
| File system / IOSU FSA       | FS* sync + async API over host directories       |
| proc_ui                      | always-foreground stub                           |
| GX2 / Latte                  | clears, scan-buffer copies, flips; no draws yet  |
| Shaders, textures, tiling    | not yet                                          |
| Audio (AX)                   | voices, final mix callbacks; effects stay dry    |
| JIT                          | Cranelift, linked blocks, translated in background |
