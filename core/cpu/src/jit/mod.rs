//! Block JIT on top of Cranelift.
//!
//! ```text
//!  Jit::run ──► block at pc translated? ──yes──► native code ──tail call──► next block
//!      ▲                │ no                          │ budget used up, target
//!      │                ▼                             ▼ not translated, or an event
//!      │        queue it for a worker, interpret      │
//!      └──────────────────┴───────────────────────────┘
//! ```
//!
//! Like Cemu's recompiler thread, translation never stops the guest: workers
//! compile in the background while the block runs on the interpreter, so new
//! code costs speed for a moment instead of a stall on the frame.
//!
//! Translating comes in two steps, because the interpreter is what makes a new scene stutter:
//! a block is first translated as quickly as possible. When nothing more urgent is left, it is
//! translated again with Cranelift's optimizations, and that code takes over. Blocks find each
//! other through the page table, so the next time they jump there they get the faster code.
//!
//! Blocks jump straight to each other: a single dispatch loop would funnel every
//! guest branch through one host call site, which the host CPU cannot predict.
//!
//! Translated instructions operate on the [`CpuState`] directly and access
//! guest memory through [`Bus::fast_memory`]. Every instruction `emit` does not
//! know falls back to one [`step`] of the reference interpreter, so the JIT
//! is an optimisation only: both must behave identically.
mod emit;

use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use cranelift_codegen::{
    ir::{AbiParam, InstBuilder, Signature, Type, types::I32},
    isa::{CallConv, OwnedTargetIsa},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use self::emit::{Job, Translated};
use crate::{Bus, CpuError, CpuState, StepEvent, interpreter::step};

/// Longest block; keeps time slices responsive in loop-free code.
const MAX_BLOCK_INSTRUCTIONS: u32 = 64;
const PAGE_BITS: u32 = 12;
const PAGE_ENTRIES: usize = 1 << (PAGE_BITS - 2);
/// Translation threads at most; the emulator and renderer need cores too.
const MAX_WORKERS: usize = 6;
/// Of those, how many also optimize: this can wait, and must not take cores from the guest.
const OPTIMIZING_WORKERS: usize = 2;
/// How often finished translations are taken in while running: making code executable means
/// asking the system to change page protections.
const COLLECT_INTERVAL: Duration = Duration::from_millis(1);

/// What a block's helper calls need besides the registers.
pub(crate) struct HelperContext {
    bus: *mut c_void,
    /// Why a block returned early.
    result: Option<Result<StepEvent, CpuError>>,
    /// Instructions left to run; blocks subtract theirs and stop at zero.
    remaining: i64,
}

/// Calls the block at the last argument with the tail-calling convention.
type EnterFn = unsafe extern "C" fn(*mut CpuState, *mut u8, *mut HelperContext, *const u8) -> u32;

/// Translations of one guest page; null where there is none. Native code reads this.
type Page = [*const u8; PAGE_ENTRIES];

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

/// What the workers have to do.
#[derive(Default)]
struct JobQueues {
    /// Blocks the guest runs on the interpreter right now.
    first: VecDeque<Job>,
    /// Blocks with quickly made code, to be made faster when there is time.
    optimize: VecDeque<Job>,
    closed: bool,
}

/// The queues and how the workers wait for them.
#[derive(Default)]
struct JobBoard {
    queues: Mutex<JobQueues>,
    changed: Condvar,
}

impl JobBoard {
    fn change(&self, change: impl FnOnce(&mut JobQueues)) {
        change(&mut self.queues.lock().unwrap_or_else(PoisonError::into_inner));
        self.changed.notify_all();
    }

    /// The most urgent job this worker takes, as soon as there is one; `None` when work is over.
    fn next(&self, optimizes: bool) -> Option<Job> {
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if queues.closed {
                return None;
            }
            let optimize = |queues: &mut JobQueues| {
                if optimizes { queues.optimize.pop_front() } else { None }
            };
            if let Some(job) = queues.first.pop_front().or_else(|| optimize(&mut queues)) {
                return Some(job);
            }
            queues = self.changed.wait(queues).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Translated code of one guest address space.
pub struct Jit {
    module: JITModule,
    enter: EnterFn,
    /// Two-level table: guest page, then instruction within the page.
    pages: Vec<Option<Box<Page>>>,
    /// Instruction count of blocks that are queued, or that failed to translate.
    untranslated: HashMap<u32, u32>,
    /// Blocks being translated for the first time, to optimize once that is done.
    awaiting: HashMap<u32, Job>,
    board: Arc<JobBoard>,
    finished: Receiver<Translated>,
    last_collect: Instant,
    /// Bumped on invalidation: translations of older code are dropped on arrival.
    generation: u64,
    /// Waits for each translation instead of interpreting meanwhile.
    synchronous: bool,
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

/// The block's calling convention: every block ends in a jump to the next one.
fn block_signature(pointer: Type) -> Signature {
    let mut signature = Signature::new(CallConv::Tail);
    signature.params.extend([AbiParam::new(pointer); 3]);
    signature.returns.push(AbiParam::new(I32));
    signature
}

/// The way from Rust into the tail-calling convention of the blocks.
#[allow(clippy::expect_used, reason = "these few fixed instructions always compile")]
fn compile_enter(module: &mut JITModule, isa: &OwnedTargetIsa) -> EnterFn {
    let pointer = isa.pointer_type();
    let mut signature = Signature::new(isa.default_call_conv());
    signature.params.extend([AbiParam::new(pointer); 4]);
    signature.returns.push(AbiParam::new(I32));
    let id = module
        .declare_function("shocora_enter_block", Linkage::Local, &signature)
        .expect("a fresh module has no such function");
    let mut context = module.make_context();
    context.func.signature = signature;
    let mut builder_context = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let params = b.block_params(entry).to_vec();
        let block = b.import_signature(block_signature(pointer));
        let call = b.ins().call_indirect(block, params[3], &params[..3]);
        let stop = b.inst_results(call)[0];
        b.ins().return_(&[stop]);
        b.finalize(isa.frontend_config());
    }
    module.define_function(id, &mut context).expect("valid function");
    module.finalize_definitions().expect("memory for a few instructions");
    let address = module.get_finalized_function(id);
    // SAFETY: `address` was just generated with exactly this signature.
    unsafe { std::mem::transmute::<*const u8, EnterFn>(address) }
}

/// Starts the threads translating blocks.
fn spawn_workers(
    isa: &OwnedTargetIsa,
    optimizing_isa: &OwnedTargetIsa,
    pages: usize,
    board: &Arc<JobBoard>,
    finished: &Sender<Translated>,
) {
    let count = std::thread::available_parallelism()
        .map_or(1, |n| n.get().saturating_sub(4))
        .clamp(1, MAX_WORKERS);
    for index in 0..count {
        let (isa, optimizing_isa) = (Arc::clone(isa), Arc::clone(optimizing_isa));
        let (board, finished) = (Arc::clone(board), finished.clone());
        let spawned = std::thread::Builder::new().name("shocora-jit".into()).spawn(move || {
            let new_worker =
                || emit::Worker::new(Arc::clone(&isa), Arc::clone(&optimizing_isa), pages);
            let mut worker = new_worker();
            while let Some(job) = board.next(index < OPTIMIZING_WORKERS) {
                // A panic in the compiler must not lose the job: its block stays
                // interpreted, and whoever waits for it hears about that.
                let translated = catch_unwind(AssertUnwindSafe(|| worker.translate(&job)))
                    .unwrap_or_else(|_| {
                        worker = new_worker();
                        job.failed()
                    });
                if finished.send(translated).is_err() {
                    break;
                }
            }
        });
        if let Err(error) = spawned {
            log::error!("cannot start a JIT thread: {error}");
        }
    }
}

/// The compiler for the host at an optimization level of Cranelift.
#[allow(
    clippy::expect_used,
    reason = "no host without Cranelift support can run shocora at usable speed"
)]
fn host_isa(level: &str) -> OwnedTargetIsa {
    let mut flags = settings::builder();
    flags.set("opt_level", level).expect("known flag");
    flags.set("use_colocated_libcalls", "false").expect("known flag");
    flags.set("is_pic", "false").expect("known flag");
    // Cranelift's tail calls rely on them.
    flags.set("preserve_frame_pointers", "true").expect("known flag");
    let verify = if cfg!(debug_assertions) { "true" } else { "false" };
    flags.set("enable_verifier", verify).expect("known flag");
    cranelift_native::builder()
        .expect("host supported by Cranelift")
        .finish(settings::Flags::new(flags))
        .expect("valid flags")
}

impl Jit {
    /// Creates an empty code cache for the host CPU.
    ///
    /// # Panics
    /// If Cranelift does not support the host (shocora only targets ones it does).
    pub fn new() -> Self {
        let isa = host_isa("none");
        let optimizing_isa = host_isa("speed");
        let mut module = JITModule::new(JITBuilder::with_isa(
            Arc::clone(&isa),
            cranelift_module::default_libcall_names(),
        ));
        let enter = compile_enter(&mut module, &isa);
        let pages: Vec<Option<Box<Page>>> =
            (0..1_usize << (32 - PAGE_BITS)).map(|_| None).collect();
        let board = Arc::new(JobBoard::default());
        let (finished_sender, finished) = mpsc::channel();
        spawn_workers(&isa, &optimizing_isa, pages.as_ptr() as usize, &board, &finished_sender);
        Self {
            module,
            enter,
            pages,
            untranslated: HashMap::new(),
            awaiting: HashMap::new(),
            board,
            finished,
            last_collect: Instant::now(),
            generation: 0,
            synchronous: std::env::var_os("SHOCORA_JIT_SYNC").is_some(),
            compiled_blocks: 0,
            compile_nanos: 0,
        }
    }

    /// Whether to wait for each translation instead of interpreting meanwhile.
    pub const fn set_synchronous(&mut self, synchronous: bool) {
        self.synchronous = synchronous;
    }

    /// Host time spent translating so far, summed over the workers.
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
        self.generation += 1;
        self.untranslated.clear();
        self.awaiting.clear();
        let end = address.saturating_add(size);
        // What is waiting for optimizing elsewhere is still good, and would otherwise stay slow.
        let generation = self.generation;
        self.board.change(|queues| {
            queues.optimize.retain_mut(|job| {
                job.set_generation(generation);
                !job.overlaps(address, end)
            });
        });
        let mut at = address & !3;
        while at < end {
            if let Some(page) = &mut self.pages[(at >> PAGE_BITS) as usize] {
                page[((at >> 2) as usize) & (PAGE_ENTRIES - 1)] = std::ptr::null();
            }
            at = at.saturating_add(4);
            if at == u32::MAX {
                break;
            }
        }
    }

    #[inline]
    fn lookup(&self, pc: u32) -> Option<*const u8> {
        let code = self.pages[(pc >> PAGE_BITS) as usize].as_ref()?
            [((pc >> 2) as usize) & (PAGE_ENTRIES - 1)];
        (!code.is_null()).then_some(code)
    }

    fn insert(&mut self, pc: u32, code: *const u8) {
        let page = self.pages[(pc >> PAGE_BITS) as usize]
            .get_or_insert_with(|| Box::new([std::ptr::null(); PAGE_ENTRIES]));
        page[((pc >> 2) as usize) & (PAGE_ENTRIES - 1)] = code;
    }

    /// Hands the code of a finished translation to the module.
    fn define(&mut self, translated: &Translated) -> Option<FuncId> {
        self.compile_nanos += translated.nanos;
        if translated.generation != self.generation {
            return None;
        }
        let Some(code) = &translated.code else {
            log::error!("JIT failed at {:#010x}, it stays interpreted", translated.pc);
            return None;
        };
        let signature = block_signature(self.module.isa().pointer_type());
        let id = self.module.declare_anonymous_function(&signature).ok()?;
        self.module.define_function_bytes(id, translated.alignment, code, &[]).ok()?;
        Some(id)
    }

    /// Takes in what the workers finished. Everything becomes executable at once:
    /// changing page protection per block would be a large part of translating.
    fn collect(&mut self, first: Option<Translated>) {
        self.last_collect = Instant::now();
        let batch: Vec<_> = first.into_iter().chain(self.finished.try_iter()).collect();
        if batch.is_empty() {
            return;
        }
        let defined: Vec<_> =
            batch.iter().filter_map(|t| Some((t.pc, t.optimized, self.define(t)?))).collect();
        if let Err(error) = self.module.finalize_definitions() {
            log::error!("JIT: cannot make code executable: {error}");
            return;
        }
        let mut to_optimize = Vec::new();
        for (pc, optimized, id) in defined {
            let code = self.module.get_finalized_function(id);
            self.insert(pc, code);
            if !optimized {
                self.untranslated.remove(&pc);
                self.compiled_blocks += 1;
                to_optimize.extend(self.awaiting.remove(&pc).map(Job::into_optimized));
            }
        }
        if !to_optimize.is_empty() {
            self.board.change(|queues| queues.optimize.extend(to_optimize));
        }
    }

    /// Takes in what the workers finished, unless that was just done.
    fn collect_if_due(&mut self) {
        if self.last_collect.elapsed() >= COLLECT_INTERVAL {
            self.collect(None);
        }
    }

    /// Queues the block at `pc`, unless it is on its way. Returns how many
    /// instructions to interpret meanwhile.
    fn queue<B: Bus>(&mut self, pc: u32, bus: &mut B) -> Result<u32, CpuError> {
        if let Some(count) = self.untranslated.get(&pc) {
            return Ok(*count);
        }
        let job = emit::decode_block::<B>(pc, bus, self.generation)?;
        let count = job.instructions.len() as u32;
        self.untranslated.insert(pc, count);
        self.awaiting.insert(pc, job.clone());
        self.board.change(|queues| queues.first.push_back(job));
        Ok(count)
    }

    /// Waits until the block at `pc` is translated, or known not to translate.
    fn wait_for(&mut self, pc: u32) {
        while self.lookup(pc).is_none() {
            let Ok(translated) = self.finished.recv() else { return };
            let failed = translated.pc == pc && translated.code.is_none() && !translated.optimized;
            self.collect(Some(translated));
            if failed {
                return;
            }
        }
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
        self.collect_if_due();
        let budget = budget as i64;
        let mut context =
            HelperContext { bus: std::ptr::from_mut(bus).cast(), result: None, remaining: budget };
        while context.remaining > 0 {
            let pc = state.pc;
            if let Some(code) = self.lookup(pc) {
                // SAFETY: the code was generated for this `CpuState` layout, and
                // `memory` is the base of the guest's flat address space.
                let stop = unsafe {
                    (self.enter)(std::ptr::from_mut(state), memory, &raw mut context, code)
                };
                if stop != 0 {
                    if let Some(result) = context.result.take() {
                        return result.map(|event| (event, (budget - context.remaining) as u64));
                    }
                }
                continue;
            }
            // SAFETY: `context.bus` is the live `bus`; nothing else uses it meanwhile.
            let bus = unsafe { &mut *context.bus.cast::<B>() };
            let count = self.queue(pc, bus)?;
            self.collect_if_due();
            if self.synchronous {
                self.wait_for(pc);
                if self.lookup(pc).is_some() {
                    continue;
                }
            }
            if let Some(event) = self.interpret(state, bus, count, &mut context)? {
                return Ok((event, (budget - context.remaining) as u64));
            }
        }
        Ok((StepEvent::Continue, (budget - context.remaining) as u64))
    }

    /// Steps through an untranslated block until it ends or translated code is reached.
    fn interpret<B: Bus>(
        &self,
        state: &mut CpuState,
        bus: &mut B,
        count: u32,
        context: &mut HelperContext,
    ) -> Result<Option<StepEvent>, CpuError> {
        for _ in 0..count {
            let next = state.pc.wrapping_add(4);
            let event = step(state, bus)?;
            context.remaining -= 1;
            if event != StepEvent::Continue {
                return Ok(Some(event));
            }
            if state.pc != next || self.lookup(state.pc).is_some() {
                break;
            }
        }
        Ok(None)
    }
}

impl Drop for Jit {
    fn drop(&mut self) {
        self.board.change(|queues| queues.closed = true);
    }
}

impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}
