//! Booting titles: mount volumes, load the RPX, set up the main thread.
use std::path::Path;

use wiiu_cafe_os::{BootInfo, CafeOs};
use wiiu_cpu::CpuState;
use wiiu_fs::{HostDirectory, Overlay, Vfs};
use wiiu_gpu::Gpu;
use wiiu_iosu::Iosu;
use wiiu_memory::Memory;
use wiiu_title::{TitleInfo, TitleSource, update};

use super::{BootSource, CORE_COUNT, EmuStats, Emulator, MAIN_CORE, Status};
use crate::EmuError;

impl Emulator {
    /// Boots a title directory or `.rpx` file.
    ///
    /// # Errors
    /// Fails if the path is not a bootable title or the RPX does not load.
    pub fn load_title(&mut self, path: &Path) -> Result<(), EmuError> {
        let mut info = TitleSource::open(path)?;
        let installed =
            info.meta.title_id.and_then(|id| update::find_update(id, &self.config.mlc_dirs));
        if let Some(dir) = installed {
            info = info.with_update(&dir)?;
            log::info!("update {}: version {:?}", dir.display(), info.meta.title_version);
        }
        let rpx = std::fs::read(&info.rpx_path)
            .map_err(|source| EmuError::Io { path: info.rpx_path.clone(), source })?;
        let vfs = self.mount_volumes(&info)?;
        let name = info
            .rpx_path
            .file_name()
            .map_or_else(|| "main.rpx".into(), |n| n.to_string_lossy().into_owned());
        self.boot(&name, &rpx, Iosu::new(vfs))?;
        self.title = Some(info);
        self.boot_source = Some(BootSource::Title(path.to_owned()));
        if let Err(error) = self.reload_cheats() {
            log::warn!("{error}");
        }
        Ok(())
    }

    /// Reboots what was booted last, like the console's reset; `false` if nothing was.
    ///
    /// # Errors
    /// Fails if the title does not load any more.
    pub fn reset(&mut self) -> Result<bool, EmuError> {
        match self.boot_source.clone() {
            Some(BootSource::Title(path)) => self.load_title(&path)?,
            Some(BootSource::Rpx(name, rpx)) => self.load_rpx(&name, &rpx)?,
            None => return Ok(false),
        }
        Ok(true)
    }

    /// Boots an in-memory RPX with no file system (tests, tools).
    ///
    /// # Errors
    /// Fails if the RPX does not load.
    pub fn load_rpx(&mut self, name: &str, rpx: &[u8]) -> Result<(), EmuError> {
        self.boot(name, rpx, Iosu::default())?;
        self.title = None;
        self.boot_source = Some(BootSource::Rpx(name.to_owned(), rpx.to_vec()));
        Ok(())
    }

    fn mount_volumes(&self, info: &TitleInfo) -> Result<Vfs, EmuError> {
        // An installed update's files lie over the title's.
        let volume = |base: &Path, name: &str| {
            let update = info.update_dir.as_ref().map(|dir| dir.join(name)).filter(|d| d.is_dir());
            update
                .into_iter()
                .fold(Overlay::new(), |overlay, dir| overlay.layer(HostDirectory::read_only(dir)))
                .layer(HostDirectory::read_only(base))
        };
        let mut vfs = Vfs::new();
        vfs.mount("/vol/code", volume(&info.code_dir, "code"))?;
        if let Some(content) = &info.content_dir {
            let mut content = volume(content, "content");
            let gate = info.meta.title_id.and_then(update::version_gate);
            if let Some(gate) =
                gate.filter(|_| info.update_dir.is_none() && self.config.skip_update_check)
            {
                log::info!("no update installed: skipping the title's update check");
                content = content.file(gate.file, gate.contents)?;
            }
            vfs.mount("/vol/content", content)?;
        }
        if let Some(meta) = &info.meta_dir {
            vfs.mount("/vol/meta", volume(meta, "meta"))?;
        }
        let title_id =
            info.meta.title_id.map_or_else(|| "homebrew".to_owned(), |id| format!("{id:016x}"));
        vfs.mount("/vol/save", HostDirectory::writable(self.config.save_dir.join(title_id)))?;
        Ok(vfs)
    }

    /// Resets the machine and starts `rpx` on the main core.
    fn boot(&mut self, name: &str, rpx: &[u8], iosu: Iosu) -> Result<(), EmuError> {
        let gamepad = self.os.gamepad;
        let pro_controllers = self.os.pro_controllers;
        self.memory = Memory::with_game_layout()?;
        self.cores = (0..CORE_COUNT as u32).map(CpuState::new).collect();
        self.jit = wiiu_cpu::Jit::new();
        self.os = CafeOs::new();
        self.os.timebase.set_speed(self.speed);
        self.os.gamepad = gamepad;
        self.os.pro_controllers = pro_controllers;
        self.os.gx2.forced_swap_interval = self.config.swap_interval;
        self.os.gx2.min_swap_interval = self.config.min_swap_interval.unwrap_or(0);
        self.os.coreinit.shared_data.fonts_dir.clone_from(&self.config.fonts_dir);
        self.gpu = Gpu::new();
        self.gpu.reset_renderer();
        self.gpu.set_resolution_factor(self.config.resolution_factor);
        self.iosu = iosu;
        self.stats = EmuStats::default();
        self.next_vsync = None;

        let boot = self.os.boot(&mut self.memory, name, rpx)?;
        start_main_thread(&mut self.cores[MAIN_CORE], &boot);
        self.gpu.blank_screens();
        self.status = Status::Running;
        Ok(())
    }
}

/// Register state at the entry point (`__preinit_user` or the title entry).
fn start_main_thread(cpu: &mut CpuState, boot: &BootInfo) {
    cpu.pc = boot.entry;
    cpu.lr = boot.return_address;
    cpu.gpr[1] = boot.stack_pointer;
    cpu.gpr[2] = boot.sda2_base;
    cpu.gpr[13] = boot.sda_base;
    cpu.gpr[3] = boot.args[0];
    cpu.gpr[4] = boot.args[1];
    cpu.gpr[5] = boot.args[2];
    // Paired-single quantisation presets coreinit installs (UGQR2..=5).
    cpu.gqr[2] = 0x0004_0004;
    cpu.gqr[3] = 0x0005_0005;
    cpu.gqr[4] = 0x0006_0006;
    cpu.gqr[5] = 0x0007_0007;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn rejects_garbage_rpx() {
        let mut emu = Emulator::new(Config::default()).unwrap();
        assert!(emu.load_rpx("bad.rpx", b"not an elf").is_err());
        assert_eq!(*emu.status(), Status::Idle);
    }
}
