//! Block JIT on top of Cranelift.
//!
//! ```text
//!  Jit::run ──► block at pc cached? ──no──► emit::compile (decode until a branch)
//!      ▲                │ yes
//!      │                ▼
//!      └── next pc ◄── native code ──► interpreter::step (anything not translated)
//! ```
//!
//! Translated instructions operate on the [`CpuState`] directly and access
//! guest memory through [`Bus::fast_memory`]. Every instruction `emit` does not
//! know falls back to one [`step`] of the reference interpreter, so the JIT
//! is an optimisation only: both must behave identically.
mod emit;

use std::ffi::c_void;

use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

use crate::{Bus, CpuError, CpuState, StepEvent, interpreter::step};

/// Longest block; keeps time slices responsive in loop-free code.
const MAX_BLOCK_INSTRUCTIONS: u32 = 64;
const PAGE_BITS: u32 = 12;
const PAGE_ENTRIES: usize = 1 << (PAGE_BITS - 2);

/// What a block's helper calls need besides the registers.
pub(crate) struct HelperContext {
    bus: *mut c_void,
    /// Why a block returned early.
    result: Option<Result<StepEvent, CpuError>>,
}

type BlockFn = unsafe extern "C" fn(*mut CpuState, *mut u8, *mut HelperContext) -> u32;

/// One instruction of the reference interpreter, called from native code.
/// Returns non-zero when the block must stop.
unsafe extern "C" fn interpret_one<B: Bus>(
    state: *mut CpuState,
    context: *mut HelperContext,
) -> u32 {
    // SAFETY: `Jit::run` passes live, exclusive pointers for the duration of the block.
    let (state, context) = unsafe { (&mut *state, &mut *context) };
    // SAFETY: `bus` was erased from a `&mut B` in `Jit::run`.
    let bus = unsafe { &mut *context.bus.cast::<B>() };
    match step(state, bus) {
        Ok(StepEvent::Continue) => 0,
        other => {
            context.result = Some(other);
            1
        }
    }
}

#[derive(Clone, Copy)]
struct Block {
    code: BlockFn,
    instructions: u32,
}

/// Translated code of one guest address space.
pub struct Jit {
    module: JITModule,
    context: cranelift_codegen::Context,
    builder_context: FunctionBuilderContext,
    /// Two-level table: guest page, then instruction within the page.
    pages: Vec<Option<Box<[Option<Block>; PAGE_ENTRIES]>>>,
    compiled_blocks: u64,
    compile_nanos: u64,
}

// SAFETY: the module's raw code pointers are only used by the thread owning the `Jit`.
unsafe impl Send for Jit {}

impl std::fmt::Debug for Jit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jit")
            .field("compiled_blocks", &self.compiled_blocks)
            .finish_non_exhaustive()
    }
}

impl Jit {
    /// Creates an empty code cache for the host CPU.
    ///
    /// # Panics
    /// If Cranelift does not support the host (flora only targets ones it does).
    #[allow(
        clippy::expect_used,
        reason = "no host without Cranelift support can run flora at usable speed"
    )]
    pub fn new() -> Self {
        let mut flags = settings::builder();
        flags.set("opt_level", if std::env::var_os("FLORA_JIT_NONE").is_some() { "none" } else { "speed" }).expect("known flag");
        flags.set("use_colocated_libcalls", "false").expect("known flag");
        flags.set("is_pic", "false").expect("known flag");
        let isa = cranelift_native::builder()
            .expect("host supported by Cranelift")
            .finish(settings::Flags::new(flags))
            .expect("valid flags");
        let module =
            JITModule::new(JITBuilder::with_isa(isa, cranelift_module::default_libcall_names()));
        Self {
            context: module.make_context(),
            module,
            builder_context: FunctionBuilderContext::new(),
            pages: (0..1_usize << (32 - PAGE_BITS)).map(|_| None).collect(),
            compiled_blocks: 0,
            compile_nanos: 0,
        }
    }

    /// Host time spent translating so far.
    pub const fn compile_nanos(&self) -> u64 {
        self.compile_nanos
    }

    /// Number of blocks translated so far.
    pub const fn compiled_blocks(&self) -> u64 {
        self.compiled_blocks
    }

    /// Forgets translations of `[address, address + size)` (self-modifying code,
    /// `ICInvalidateRange`).
    pub fn invalidate(&mut self, address: u32, size: u32) {
        let end = address.saturating_add(size);
        let mut at = address & !3;
        while at < end {
            if let Some(page) = &mut self.pages[(at >> PAGE_BITS) as usize] {
                page[((at >> 2) as usize) & (PAGE_ENTRIES - 1)] = None;
            }
            at = at.saturating_add(4);
            if at == u32::MAX {
                break;
            }
        }
    }

    #[inline]
    fn lookup(&self, pc: u32) -> Option<Block> {
        self.pages[(pc >> PAGE_BITS) as usize].as_ref()?[((pc >> 2) as usize) & (PAGE_ENTRIES - 1)]
    }

    fn insert(&mut self, pc: u32, block: Block) {
        let page = self.pages[(pc >> PAGE_BITS) as usize]
            .get_or_insert_with(|| Box::new([None; PAGE_ENTRIES]));
        page[((pc >> 2) as usize) & (PAGE_ENTRIES - 1)] = Some(block);
    }

    /// Runs until an event other than [`StepEvent::Continue`] occurs or at
    /// least `budget` instructions have executed. Falls back to the interpreter
    /// when the bus has no flat memory.
    ///
    /// # Errors
    /// See [`step`].
    pub fn run<B: Bus>(
        &mut self,
        state: &mut CpuState,
        bus: &mut B,
        budget: u64,
    ) -> Result<(StepEvent, u64), CpuError> {
        let Some(memory) = bus.fast_memory() else {
            return crate::interpreter::run(state, bus, budget);
        };
        let mut context = HelperContext { bus: std::ptr::from_mut(bus).cast(), result: None };
        let mut executed = 0;
        while executed < budget {
            let pc = state.pc;
            let block = match self.lookup(pc) {
                Some(block) => block,
                None => {
                    // SAFETY: `context.bus` is the live `bus`; nothing else uses it meanwhile.
                    let bus = unsafe { &mut *context.bus.cast::<B>() };
                    let timer = std::time::Instant::now();
                    let block = self.compile(pc, bus)?;
                    self.compile_nanos += timer.elapsed().as_nanos() as u64;
                    block
                }
            };
            // SAFETY: the code was generated for this `CpuState` layout, and
            // `memory` is the base of the guest's flat address space.
            let stop = unsafe { (block.code)(std::ptr::from_mut(state), memory, &raw mut context) };
            executed += u64::from(block.instructions);
            if stop != 0 {
                if let Some(result) = context.result.take() {
                    return result.map(|event| (event, executed));
                }
            }
        }
        Ok((StepEvent::Continue, executed))
    }
}

impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}
