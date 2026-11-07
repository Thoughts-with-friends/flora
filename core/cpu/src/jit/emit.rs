//! Guest instructions → Cranelift IR.
//!
//! A block is a run of code with one entry and side exits: registers are cached in
//! SSA values while translating and written back before anything that can observe
//! them (block exits and interpreter fallbacks). No merge points exist, so no
//! variables or block parameters are needed: an exit is a dead end for the code
//! after it, which goes on with the values from before the exit.
//!
//! Translation runs on worker threads and only needs the instructions, which
//! the owner of the bus decodes up front ([`decode_block`]).
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]

use std::mem::offset_of;

use cranelift_codegen::{
    Context,
    control::ControlPlane,
    ir::{
        AbiParam, Block as IrBlock, InstBuilder, MemFlagsData, SigRef, Signature, Value,
        condcodes::{FloatCC, IntCC},
        types::{F32, F64, I8, I16, I32, I64},
    },
    isa::OwnedTargetIsa,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use super::{
    HelperContext, MAX_BLOCK_INSTRUCTIONS, PAGE_BITS, PAGE_ENTRIES, block_signature, interpret_one,
};
use crate::{
    Bus, CpuError, CpuState, Fpr, Instr, Opcode, decode, interpreter::integer::rotate_mask,
    state::spr,
};

/// Whether translation of the block goes on after an instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Next,
    /// `state.pc` is already written and the block returned.
    Ended,
    /// Run the interpreter for this instruction.
    Fallback,
}

const fn gpr_offset(index: usize) -> i32 {
    (offset_of!(CpuState, gpr) + 4 * index) as i32
}

const fn fpr_offset(index: usize, slot: usize) -> i32 {
    let field = if slot == 0 { offset_of!(Fpr, ps0) } else { offset_of!(Fpr, ps1) };
    (offset_of!(CpuState, fpr) + size_of::<Fpr>() * index + field) as i32
}

const fn field_offset_lr() -> i32 {
    offset_of!(CpuState, lr) as i32
}

/// Everything cached at one point of the translation.
#[derive(Clone, Copy)]
struct Cached {
    gpr: [Option<Value>; 32],
    gpr_dirty: u32,
    fpr: [[Option<Value>; 2]; 32],
    fpr_dirty: [u32; 2],
    lr: Option<Value>,
    ctr: Option<Value>,
    cr: Option<Value>,
    ca: Option<Value>,
    dirty: [bool; 4],
}

struct Emit<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    state: Value,
    memory: Value,
    helper_context: Value,
    interpret: Value,
    interpret_sig: SigRef,
    block_sig: SigRef,
    /// Address of the first-level table of translations.
    pages: usize,
    /// Guest instructions up to and including the one being translated.
    executed: i64,
    gpr: [Option<Value>; 32],
    gpr_dirty: u32,
    fpr: [[Option<Value>; 2]; 32],
    fpr_dirty: [u32; 2],
    lr: Option<Value>,
    ctr: Option<Value>,
    cr: Option<Value>,
    /// XER[CA] as an `i32` 0/1.
    ca: Option<Value>,
    lr_dirty: bool,
    ctr_dirty: bool,
    cr_dirty: bool,
    ca_dirty: bool,
}

fn flags() -> MemFlagsData {
    MemFlagsData::new().with_notrap()
}

impl Emit<'_, '_> {
    fn c32(&mut self, value: u32) -> Value {
        self.b.ins().iconst(I32, i64::from(value as i32))
    }

    fn load_state(&mut self, offset: i32) -> Value {
        self.b.ins().load(I32, flags(), self.state, offset)
    }

    fn store_state(&mut self, offset: i32, value: Value) {
        self.b.ins().store(flags(), value, self.state, offset);
    }

    fn gpr(&mut self, index: usize) -> Value {
        if let Some(value) = self.gpr[index] {
            return value;
        }
        let value = self.load_state(gpr_offset(index));
        self.gpr[index] = Some(value);
        value
    }

    fn set_gpr(&mut self, index: usize, value: Value) {
        self.gpr[index] = Some(value);
        self.gpr_dirty |= 1 << index;
    }

    /// `(rA|0)`.
    fn ra_or_zero(&mut self, index: usize) -> Value {
        if index == 0 { self.c32(0) } else { self.gpr(index) }
    }

    fn fpr(&mut self, index: usize, slot: usize) -> Value {
        if let Some(value) = self.fpr[index][slot] {
            return value;
        }
        let value = self.b.ins().load(F64, flags(), self.state, fpr_offset(index, slot));
        self.fpr[index][slot] = Some(value);
        value
    }

    fn set_fpr(&mut self, index: usize, slot: usize, value: Value) {
        self.fpr[index][slot] = Some(value);
        self.fpr_dirty[slot] |= 1 << index;
    }

    fn lr(&mut self) -> Value {
        if let Some(value) = self.lr {
            return value;
        }
        let value = self.load_state(field_offset_lr());
        self.lr = Some(value);
        value
    }

    fn set_lr(&mut self, value: Value) {
        self.lr = Some(value);
        self.lr_dirty = true;
    }

    fn ctr(&mut self) -> Value {
        if let Some(value) = self.ctr {
            return value;
        }
        let value = self.load_state(offset_of!(CpuState, ctr) as i32);
        self.ctr = Some(value);
        value
    }

    fn set_ctr(&mut self, value: Value) {
        self.ctr = Some(value);
        self.ctr_dirty = true;
    }

    fn cr(&mut self) -> Value {
        if let Some(value) = self.cr {
            return value;
        }
        let value = self.load_state(offset_of!(CpuState, cr) as i32);
        self.cr = Some(value);
        value
    }

    fn set_cr(&mut self, value: Value) {
        self.cr = Some(value);
        self.cr_dirty = true;
    }

    fn ca(&mut self) -> Value {
        if let Some(value) = self.ca {
            return value;
        }
        let value =
            self.b.ins().uload8(I32, flags(), self.state, offset_of!(CpuState, xer_ca) as i32);
        self.ca = Some(value);
        value
    }

    fn set_ca(&mut self, value: Value) {
        self.ca = Some(value);
        self.ca_dirty = true;
    }

    fn so(&mut self) -> Value {
        self.b.ins().uload8(I32, flags(), self.state, offset_of!(CpuState, xer_so) as i32)
    }

    /// Writes cached registers back to `state`.
    fn flush(&mut self) {
        let mut dirty = self.gpr_dirty;
        while dirty != 0 {
            let index = dirty.trailing_zeros() as usize;
            dirty &= dirty - 1;
            if let Some(value) = self.gpr[index] {
                self.store_state(gpr_offset(index), value);
            }
        }
        self.gpr_dirty = 0;
        for slot in 0..2 {
            let mut dirty = self.fpr_dirty[slot];
            while dirty != 0 {
                let index = dirty.trailing_zeros() as usize;
                dirty &= dirty - 1;
                if let Some(value) = self.fpr[index][slot] {
                    self.store_state(fpr_offset(index, slot), value);
                }
            }
            self.fpr_dirty[slot] = 0;
        }
        if self.lr_dirty {
            if let Some(value) = self.lr {
                self.store_state(field_offset_lr(), value);
            }
            self.lr_dirty = false;
        }
        if self.ctr_dirty {
            if let Some(value) = self.ctr {
                self.store_state(offset_of!(CpuState, ctr) as i32, value);
            }
            self.ctr_dirty = false;
        }
        if self.cr_dirty {
            if let Some(value) = self.cr {
                self.store_state(offset_of!(CpuState, cr) as i32, value);
            }
            self.cr_dirty = false;
        }
        if self.ca_dirty {
            if let Some(value) = self.ca {
                let byte = self.b.ins().ireduce(I8, value);
                self.b.ins().store(flags(), byte, self.state, offset_of!(CpuState, xer_ca) as i32);
            }
            self.ca_dirty = false;
        }
    }

    /// Forgets everything cached: a helper may have changed any register.
    fn invalidate(&mut self) {
        self.gpr = [None; 32];
        self.fpr = [[None; 2]; 32];
        self.lr = None;
        self.ctr = None;
        self.cr = None;
        self.ca = None;
    }

    fn set_pc(&mut self, pc: Value) {
        self.store_state(offset_of!(CpuState, pc) as i32, pc);
    }

    /// Accounts for the instructions run so far; returns how many are left to run.
    fn charge(&mut self) -> Value {
        let offset = offset_of!(HelperContext, remaining) as i32;
        let remaining = self.b.ins().load(I64, flags(), self.helper_context, offset);
        let left = self.b.ins().iadd_imm_s(remaining, -self.executed);
        self.b.ins().store(flags(), left, self.helper_context, offset);
        left
    }

    fn snapshot(&self) -> Cached {
        Cached {
            gpr: self.gpr,
            gpr_dirty: self.gpr_dirty,
            fpr: self.fpr,
            fpr_dirty: self.fpr_dirty,
            lr: self.lr,
            ctr: self.ctr,
            cr: self.cr,
            ca: self.ca,
            dirty: [self.lr_dirty, self.ctr_dirty, self.cr_dirty, self.ca_dirty],
        }
    }

    fn restore(&mut self, cached: &Cached) {
        self.gpr = cached.gpr;
        self.gpr_dirty = cached.gpr_dirty;
        self.fpr = cached.fpr;
        self.fpr_dirty = cached.fpr_dirty;
        self.lr = cached.lr;
        self.ctr = cached.ctr;
        self.cr = cached.cr;
        self.ca = cached.ca;
        [self.lr_dirty, self.ctr_dirty, self.cr_dirty, self.ca_dirty] = cached.dirty;
    }

    /// Runs `exit` when `condition` holds, and goes on after it otherwise. Nothing
    /// `exit` caches or writes back is true for the code that goes on.
    fn side_exit(&mut self, condition: Value, exit: impl FnOnce(&mut Self)) {
        let taken = self.b.create_block();
        let stay = self.b.create_block();
        self.b.ins().brif(condition, taken, &[], stay, &[]);
        self.b.seal_block(taken);
        self.b.seal_block(stay);
        let cached = self.snapshot();
        self.b.switch_to_block(taken);
        exit(self);
        self.restore(&cached);
        self.b.switch_to_block(stay);
    }

    /// Returns to [`Jit::run`](super::Jit::run).
    fn leave(&mut self) {
        let done = self.c32(0);
        self.b.ins().return_(&[done]);
    }

    /// Leaves the block with `pc` as the next address, jumping to its translation
    /// when there is one. `known` is the address, when it is a constant.
    fn end_block(&mut self, pc: Value, known: Option<u32>) -> Flow {
        self.flush();
        self.set_pc(pc);
        let left = self.charge();
        let used_up = self.b.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, left, 0);
        self.side_exit(used_up, Self::leave);

        let page_entry = match known {
            Some(address) => {
                let entry = self.pages + 8 * (address >> PAGE_BITS) as usize;
                self.b.ins().iconst(I64, entry as i64)
            }
            None => {
                let page = self.b.ins().ushr_imm_s(pc, i64::from(PAGE_BITS));
                let page = self.b.ins().uextend(I64, page);
                let scaled = self.b.ins().ishl_imm_s(page, 3);
                let table = self.b.ins().iconst(I64, self.pages as i64);
                self.b.ins().iadd(table, scaled)
            }
        };
        let page = self.b.ins().load(I64, flags(), page_entry, 0);
        let missing = self.b.ins().icmp_imm_s(IntCC::Equal, page, 0);
        self.side_exit(missing, Self::leave);
        let slot = match known {
            Some(address) => {
                let slot = ((address >> 2) as usize & (PAGE_ENTRIES - 1)) * 8;
                self.b.ins().iadd_imm_s(page, slot as i64)
            }
            None => {
                let index = self.b.ins().ushr_imm_s(pc, 2);
                let index = self.b.ins().band_imm_s(index, (PAGE_ENTRIES - 1) as i64);
                let index = self.b.ins().uextend(I64, index);
                let scaled = self.b.ins().ishl_imm_s(index, 3);
                self.b.ins().iadd(page, scaled)
            }
        };
        let code = self.b.ins().load(I64, flags(), slot, 0);
        let missing = self.b.ins().icmp_imm_s(IntCC::Equal, code, 0);
        self.side_exit(missing, Self::leave);
        self.b.ins().return_call_indirect(
            self.block_sig,
            code,
            &[self.state, self.memory, self.helper_context],
        );
        Flow::Ended
    }

    /// One step of the interpreter; returns from the block if it reports an event.
    fn interpret(&mut self, pc: u32) {
        self.flush();
        let address = self.c32(pc);
        self.set_pc(address);
        let call = self.b.ins().call_indirect(
            self.interpret_sig,
            self.interpret,
            &[self.state, self.helper_context],
        );
        let stop = self.b.inst_results(call)[0];
        let exit: IrBlock = self.b.create_block();
        let resume: IrBlock = self.b.create_block();
        self.b.ins().brif(stop, exit, &[], resume, &[]);
        self.b.seal_block(exit);
        self.b.seal_block(resume);
        self.b.switch_to_block(exit);
        self.charge();
        self.b.ins().return_(&[stop]);
        self.b.switch_to_block(resume);
        self.invalidate();
    }

    fn host_address(&mut self, ea: Value) -> Value {
        let offset = self.b.ins().uextend(I64, ea);
        self.b.ins().iadd(self.memory, offset)
    }

    fn load32(&mut self, ea: Value) -> Value {
        let address = self.host_address(ea);
        let raw = self.b.ins().load(I32, flags(), address, 0);
        self.b.ins().bswap(raw)
    }

    fn store32(&mut self, ea: Value, value: Value) {
        let address = self.host_address(ea);
        let raw = self.b.ins().bswap(value);
        self.b.ins().store(flags(), raw, address, 0);
    }

    /// `crf = (lt, gt, eq, so)` from `i8` booleans.
    fn set_cr_field(&mut self, field: usize, value: Value) {
        let shift = 28 - 4 * field as i64;
        let cr = self.cr();
        let cleared = self.b.ins().band_imm_s(cr, !(0xF_i64 << shift));
        let placed = self.b.ins().ishl_imm_s(value, shift);
        let merged = self.b.ins().bor(cleared, placed);
        self.set_cr(merged);
    }

    fn compare_field(&mut self, lt: Value, gt: Value, eq: Value, low: Value) -> Value {
        let lt = self.b.ins().uextend(I32, lt);
        let gt = self.b.ins().uextend(I32, gt);
        let eq = self.b.ins().uextend(I32, eq);
        let lt = self.b.ins().ishl_imm_s(lt, 3);
        let gt = self.b.ins().ishl_imm_s(gt, 2);
        let eq = self.b.ins().ishl_imm_s(eq, 1);
        let high = self.b.ins().bor(lt, gt);
        let middle = self.b.ins().bor(high, eq);
        self.b.ins().bor(middle, low)
    }

    fn compare(&mut self, field: usize, a: Value, b: Value, signed: bool) {
        let (less, greater) = if signed {
            (IntCC::SignedLessThan, IntCC::SignedGreaterThan)
        } else {
            (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan)
        };
        let lt = self.b.ins().icmp(less, a, b);
        let gt = self.b.ins().icmp(greater, a, b);
        let eq = self.b.ins().icmp(IntCC::Equal, a, b);
        let so = self.so();
        let value = self.compare_field(lt, gt, eq, so);
        self.set_cr_field(field, value);
    }

    fn update_cr0(&mut self, result: Value) {
        let zero = self.c32(0);
        self.compare(0, result, zero, true);
    }

    fn record(&mut self, i: Instr, result: Value) {
        if i.rc() {
            self.update_cr0(result);
        }
    }

    /// `a + b + carry` → (result, carry out as `i32`).
    fn add_with_carry(&mut self, a: Value, b: Value, carry: Option<Value>) -> (Value, Value) {
        let wide_a = self.b.ins().uextend(I64, a);
        let wide_b = self.b.ins().uextend(I64, b);
        let mut sum = self.b.ins().iadd(wide_a, wide_b);
        if let Some(carry) = carry {
            let wide_carry = self.b.ins().uextend(I64, carry);
            sum = self.b.ins().iadd(sum, wide_carry);
        }
        let result = self.b.ins().ireduce(I32, sum);
        let high = self.b.ins().ushr_imm_s(sum, 32);
        (result, self.b.ins().ireduce(I32, high))
    }

    fn not(&mut self, value: Value) -> Value {
        self.b.ins().bnot(value)
    }

    fn round_single(&mut self, value: Value) -> Value {
        let single = self.b.ins().fdemote(F32, value);
        self.b.ins().fpromote(F64, single)
    }

    fn cr_bit(&mut self, bit: usize) -> Value {
        let cr = self.cr();
        let shifted = self.b.ins().ushr_imm_s(cr, 31 - bit as i64);
        self.b.ins().band_imm_s(shifted, 1)
    }

    /// Effective address of a D-form access; `update` ignores the `(rA|0)` rule.
    fn ea_d(&mut self, i: Instr, update: bool) -> Value {
        let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
        self.b.ins().iadd_imm_s(base, i64::from(i.simm()))
    }

    fn ea_x(&mut self, i: Instr, update: bool) -> Value {
        let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
        let index = self.gpr(i.rb());
        self.b.ins().iadd(base, index)
    }

    fn integer_load(&mut self, i: Instr, ea: Value, size: u32, signed: bool, update: bool) {
        let address = self.host_address(ea);
        let value = match size {
            4 => {
                let raw = self.b.ins().load(I32, flags(), address, 0);
                self.b.ins().bswap(raw)
            }
            2 => {
                let raw = self.b.ins().load(I16, flags(), address, 0);
                let swapped = self.b.ins().bswap(raw);
                if signed {
                    self.b.ins().sextend(I32, swapped)
                } else {
                    self.b.ins().uextend(I32, swapped)
                }
            }
            _ => self.b.ins().uload8(I32, flags(), address, 0),
        };
        self.set_gpr(i.rd(), value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn integer_store(&mut self, i: Instr, ea: Value, size: u32, update: bool) {
        let value = self.gpr(i.rd());
        let address = self.host_address(ea);
        match size {
            4 => {
                let raw = self.b.ins().bswap(value);
                self.b.ins().store(flags(), raw, address, 0);
            }
            2 => {
                let half = self.b.ins().ireduce(I16, value);
                let raw = self.b.ins().bswap(half);
                self.b.ins().store(flags(), raw, address, 0);
            }
            _ => {
                let byte = self.b.ins().ireduce(I8, value);
                self.b.ins().store(flags(), byte, address, 0);
            }
        }
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn load_single(&mut self, i: Instr, ea: Value, update: bool) {
        let raw = self.load32(ea);
        let single = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
        let value = self.b.ins().fpromote(F64, single);
        self.set_fpr(i.rd(), 0, value);
        self.set_fpr(i.rd(), 1, value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn load_double(&mut self, i: Instr, ea: Value, update: bool) {
        let address = self.host_address(ea);
        let raw = self.b.ins().load(I64, flags(), address, 0);
        let swapped = self.b.ins().bswap(raw);
        let value = self.b.ins().bitcast(F64, MemFlagsData::new(), swapped);
        self.set_fpr(i.rd(), 0, value);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn store_single(&mut self, i: Instr, ea: Value, update: bool) {
        let value = self.fpr(i.rd(), 0);
        let single = self.b.ins().fdemote(F32, value);
        let raw = self.b.ins().bitcast(I32, MemFlagsData::new(), single);
        self.store32(ea, raw);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    fn store_double(&mut self, i: Instr, ea: Value, update: bool) {
        let value = self.fpr(i.rd(), 0);
        let raw = self.b.ins().bitcast(I64, MemFlagsData::new(), value);
        let swapped = self.b.ins().bswap(raw);
        let address = self.host_address(ea);
        self.b.ins().store(flags(), swapped, address, 0);
        if update {
            self.set_gpr(i.ra(), ea);
        }
    }

    /// BO/BI of conditional branches as an `i8`-like value, `None` when always taken.
    fn branch_condition(&mut self, i: Instr, use_ctr: bool) -> Option<Value> {
        let (bo, bi) = (i.rd(), i.ra());
        let ctr_ok = (use_ctr && bo & 0b00100 == 0).then(|| {
            let ctr = self.ctr();
            let decremented = self.b.ins().iadd_imm_s(ctr, -1);
            self.set_ctr(decremented);
            let cc = if bo & 0b00010 != 0 { IntCC::Equal } else { IntCC::NotEqual };
            self.b.ins().icmp_imm_s(cc, decremented, 0)
        });
        let cond_ok = (bo & 0b10000 == 0).then(|| {
            let bit = self.cr_bit(bi);
            let want = i64::from(bo & 0b01000 != 0);
            self.b.ins().icmp_imm_s(IntCC::Equal, bit, want)
        });
        match (ctr_ok, cond_ok) {
            (Some(a), Some(b)) => Some(self.b.ins().band(a, b)),
            (a, b) => a.or(b),
        }
    }

    /// A branch: the block ends if it is always taken, and goes on with the next
    /// instruction after a conditional one.
    fn branch(
        &mut self,
        i: Instr,
        pc: u32,
        target: Value,
        known: Option<u32>,
        condition: Option<Value>,
    ) -> Flow {
        if i.lk() {
            let next = self.c32(pc.wrapping_add(4));
            self.set_lr(next);
        }
        let Some(condition) = condition else { return self.end_block(target, known) };
        self.side_exit(condition, |emit| {
            emit.end_block(target, known);
        });
        Flow::Next
    }

    /// `psq_l`/`psq_st` are translated for the common float case. With an integer
    /// type in the GQR (`type_bit` set) the block ends through the interpreter.
    fn leave_unless_float(&mut self, pc: u32, gqr: usize, type_bit: i64) {
        let offset = (offset_of!(CpuState, gqr) + 4 * gqr) as i32;
        let quant = self.load_state(offset);
        let integer = self.b.ins().band_imm_s(quant, type_bit);
        self.side_exit(integer, |emit| {
            emit.interpret(pc);
            let next = emit.c32(pc.wrapping_add(4));
            emit.end_block(next, Some(pc.wrapping_add(4)));
        });
    }

    /// Effective address of a `psq_*`, and whether the form updates `rA`.
    fn psq_ea(&mut self, i: Instr, op: Opcode) -> (Value, bool) {
        use Opcode as O;
        let update = matches!(op, O::PsqLu | O::PsqStu | O::PsqLux | O::PsqStux);
        if matches!(op, O::PsqL | O::PsqSt | O::PsqLu | O::PsqStu) {
            let base = if update { self.gpr(i.ra()) } else { self.ra_or_zero(i.ra()) };
            (self.b.ins().iadd_imm_s(base, i64::from(i.psq_d())), update)
        } else {
            (self.ea_x(i, update), update)
        }
    }

    /// Single-precision arithmetic: rounded result in both slots.
    fn set_single(&mut self, i: Instr, value: Value) {
        let rounded = self.round_single(value);
        self.set_fpr(i.rd(), 0, rounded);
        self.set_fpr(i.rd(), 1, rounded);
    }

    fn set_paired(&mut self, i: Instr, ps0: Value, ps1: Value) {
        let (ps0, ps1) = (self.round_single(ps0), self.round_single(ps1));
        self.set_fpr(i.rd(), 0, ps0);
        self.set_fpr(i.rd(), 1, ps1);
    }

    /// Arithmetic shared by the double, single and paired forms, on slot `slot`.
    fn arith(&mut self, op: Opcode, i: Instr, slot: usize) -> Option<Value> {
        use Opcode as O;
        let a = self.fpr(i.ra(), slot);
        let b = self.fpr(i.rb(), slot);
        let c = self.fpr(i.rc_reg(), slot);
        Some(match op {
            O::Fadd | O::Fadds | O::PsAdd => self.b.ins().fadd(a, b),
            O::Fsub | O::Fsubs | O::PsSub => self.b.ins().fsub(a, b),
            O::Fmul | O::Fmuls | O::PsMul => self.b.ins().fmul(a, c),
            O::Fdiv | O::Fdivs | O::PsDiv => self.b.ins().fdiv(a, b),
            O::Fmadd | O::Fmadds | O::PsMadd => self.b.ins().fma(a, c, b),
            O::Fmsub | O::Fmsubs | O::PsMsub => {
                let negated = self.b.ins().fneg(b);
                self.b.ins().fma(a, c, negated)
            }
            O::Fnmadd | O::Fnmadds | O::PsNmadd => {
                let sum = self.b.ins().fma(a, c, b);
                self.b.ins().fneg(sum)
            }
            O::Fnmsub | O::Fnmsubs | O::PsNmsub => {
                let negated = self.b.ins().fneg(b);
                let sum = self.b.ins().fma(a, c, negated);
                self.b.ins().fneg(sum)
            }
            _ => return None,
        })
    }

    /// `ps_muls*`/`ps_madds*`: both slots of `frA` scale by one slot of `frC`.
    fn scaled(&mut self, i: Instr, scale_slot: usize, add: bool) {
        let c = self.fpr(i.rc_reg(), scale_slot);
        let mut results = [None; 2];
        for (slot, result) in results.iter_mut().enumerate() {
            let a = self.fpr(i.ra(), slot);
            *result = Some(if add {
                let b = self.fpr(i.rb(), slot);
                self.b.ins().fma(a, c, b)
            } else {
                self.b.ins().fmul(a, c)
            });
        }
        if let [Some(ps0), Some(ps1)] = results {
            self.set_paired(i, ps0, ps1);
        }
    }

    fn sign_op(&mut self, op: Opcode, value: Value) -> Value {
        use Opcode as O;
        match op {
            O::Fneg | O::PsNeg => self.b.ins().fneg(value),
            O::Fabs | O::PsAbs => self.b.ins().fabs(value),
            O::Fnabs | O::PsNabs => {
                let absolute = self.b.ins().fabs(value);
                self.b.ins().fneg(absolute)
            }
            _ => value,
        }
    }

    fn float_compare(&mut self, field: usize, a: Value, b: Value) {
        let lt = self.b.ins().fcmp(FloatCC::LessThan, a, b);
        let gt = self.b.ins().fcmp(FloatCC::GreaterThan, a, b);
        let eq = self.b.ins().fcmp(FloatCC::Equal, a, b);
        let unordered = self.b.ins().fcmp(FloatCC::Unordered, a, b);
        let unordered = self.b.ins().uextend(I32, unordered);
        let value = self.compare_field(lt, gt, eq, unordered);
        self.set_cr_field(field, value);
        let fpscr = self.load_state(offset_of!(CpuState, fpscr) as i32);
        let cleared = self.b.ins().band_imm_s(fpscr, !0xF000_i64);
        let placed = self.b.ins().ishl_imm_s(value, 12);
        let merged = self.b.ins().bor(cleared, placed);
        self.store_state(offset_of!(CpuState, fpscr) as i32, merged);
    }

    /// Translates one instruction.
    fn instruction(&mut self, op: Opcode, i: Instr, pc: u32) -> Flow {
        use Opcode as O;
        let (rd, ra, rb) = (i.rd(), i.ra(), i.rb());
        match op {
            // ---- Integer immediates ----
            O::Addi => {
                let base = self.ra_or_zero(ra);
                let value = self.b.ins().iadd_imm_s(base, i64::from(i.simm()));
                self.set_gpr(rd, value);
            }
            O::Addis => {
                let base = self.ra_or_zero(ra);
                let value = self.b.ins().iadd_imm_s(base, i64::from(i.simm() << 16));
                self.set_gpr(rd, value);
            }
            O::Addic | O::AddicRc => {
                let a = self.gpr(ra);
                let imm = self.c32(i.simm() as u32);
                let (result, carry) = self.add_with_carry(a, imm, None);
                self.set_gpr(rd, result);
                self.set_ca(carry);
                if op == O::AddicRc {
                    self.update_cr0(result);
                }
            }
            O::Subfic => {
                let a = self.gpr(ra);
                let inverted = self.not(a);
                let imm = self.c32(i.simm() as u32);
                let one = self.c32(1);
                let (result, carry) = self.add_with_carry(inverted, imm, Some(one));
                self.set_gpr(rd, result);
                self.set_ca(carry);
            }
            O::Mulli => {
                let a = self.gpr(ra);
                let imm = self.c32(i.simm() as u32);
                let value = self.b.ins().imul(a, imm);
                self.set_gpr(rd, value);
            }
            O::Cmpi => {
                let a = self.gpr(ra);
                let imm = self.c32(i.simm() as u32);
                self.compare(i.crfd(), a, imm, true);
            }
            O::Cmpli => {
                let a = self.gpr(ra);
                let imm = self.c32(i.uimm());
                self.compare(i.crfd(), a, imm, false);
            }
            O::Ori | O::Oris | O::Xori | O::Xoris | O::AndiRc | O::AndisRc => {
                let s = self.gpr(rd);
                let imm = if matches!(op, O::Oris | O::Xoris | O::AndisRc) {
                    i.uimm() << 16
                } else {
                    i.uimm()
                };
                let imm = i64::from(imm as i32);
                let value = match op {
                    O::Ori | O::Oris => self.b.ins().bor_imm_s(s, imm),
                    O::Xori | O::Xoris => self.b.ins().bxor_imm_s(s, imm),
                    _ => self.b.ins().band_imm_s(s, imm),
                };
                self.set_gpr(ra, value);
                if matches!(op, O::AndiRc | O::AndisRc) {
                    self.update_cr0(value);
                }
            }

            // ---- Rotates ----
            O::Rlwinm => {
                let s = self.gpr(rd);
                let rotated =
                    if i.sh() == 0 { s } else { self.b.ins().rotl_imm_s(s, i64::from(i.sh())) };
                let value =
                    self.b.ins().band_imm_s(rotated, i64::from(rotate_mask(i.mb(), i.me()) as i32));
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Rlwnm => {
                let s = self.gpr(rd);
                let amount = self.gpr(rb);
                let amount = self.b.ins().band_imm_s(amount, 31);
                let rotated = self.b.ins().rotl(s, amount);
                let value =
                    self.b.ins().band_imm_s(rotated, i64::from(rotate_mask(i.mb(), i.me()) as i32));
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Rlwimi => {
                let s = self.gpr(rd);
                let old = self.gpr(ra);
                let mask = rotate_mask(i.mb(), i.me());
                let rotated =
                    if i.sh() == 0 { s } else { self.b.ins().rotl_imm_s(s, i64::from(i.sh())) };
                let inserted = self.b.ins().band_imm_s(rotated, i64::from(mask as i32));
                let kept = self.b.ins().band_imm_s(old, i64::from(!mask as i32));
                let value = self.b.ins().bor(inserted, kept);
                self.set_gpr(ra, value);
                self.record(i, value);
            }

            // ---- XO-form arithmetic (overflow-enabled forms use the interpreter) ----
            O::Add
            | O::Addc
            | O::Adde
            | O::Addze
            | O::Addme
            | O::Subf
            | O::Subfc
            | O::Subfe
            | O::Subfze
            | O::Subfme
            | O::Neg
                if i.oe() =>
            {
                return Flow::Fallback;
            }
            O::Mullw if i.oe() => return Flow::Fallback,
            O::Add => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().iadd(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Subf => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().isub(b, a);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Neg => {
                let a = self.gpr(ra);
                let value = self.b.ins().ineg(a);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Addc
            | O::Adde
            | O::Addze
            | O::Addme
            | O::Subfc
            | O::Subfe
            | O::Subfze
            | O::Subfme => {
                let a = self.gpr(ra);
                let first = if matches!(op, O::Addc | O::Adde | O::Addze | O::Addme) {
                    a
                } else {
                    self.not(a)
                };
                let second = match op {
                    O::Addc | O::Adde | O::Subfc | O::Subfe => self.gpr(rb),
                    O::Addze | O::Subfze => self.c32(0),
                    _ => self.c32(u32::MAX),
                };
                let carry_in = match op {
                    O::Addc => None,
                    O::Subfc => Some(self.c32(1)),
                    _ => Some(self.ca()),
                };
                let (result, carry) = self.add_with_carry(first, second, carry_in);
                self.set_gpr(rd, result);
                self.set_ca(carry);
                self.record(i, result);
            }
            O::Mullw => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().imul(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Mulhw => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().smulhi(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Mulhwu => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                let value = self.b.ins().umulhi(a, b);
                self.set_gpr(rd, value);
                self.record(i, value);
            }
            O::Cmp => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                self.compare(i.crfd(), a, b, true);
            }
            O::Cmpl => {
                let a = self.gpr(ra);
                let b = self.gpr(rb);
                self.compare(i.crfd(), a, b, false);
            }

            // ---- Logic ----
            O::And | O::Andc | O::Or | O::Orc | O::Xor | O::Nor | O::Nand | O::Eqv => {
                let s = self.gpr(rd);
                let b = self.gpr(rb);
                let value = match op {
                    O::And => self.b.ins().band(s, b),
                    O::Andc => self.b.ins().band_not(s, b),
                    O::Or => self.b.ins().bor(s, b),
                    O::Orc => self.b.ins().bor_not(s, b),
                    O::Xor => self.b.ins().bxor(s, b),
                    O::Nor => {
                        let v = self.b.ins().bor(s, b);
                        self.not(v)
                    }
                    O::Nand => {
                        let v = self.b.ins().band(s, b);
                        self.not(v)
                    }
                    _ => self.b.ins().bxor_not(s, b),
                };
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Slw | O::Srw => {
                let s = self.gpr(rd);
                let amount = self.gpr(rb);
                let amount = self.b.ins().band_imm_s(amount, 0x3F);
                let wide = self.b.ins().uextend(I64, s);
                let wide_amount = self.b.ins().uextend(I64, amount);
                let shifted = if op == O::Slw {
                    self.b.ins().ishl(wide, wide_amount)
                } else {
                    self.b.ins().ushr(wide, wide_amount)
                };
                let value = self.b.ins().ireduce(I32, shifted);
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Sraw => {
                let s = self.gpr(rd);
                let amount = self.gpr(rb);
                let amount = self.b.ins().band_imm_s(amount, 0x3F);
                let wide_amount = self.b.ins().uextend(I64, amount);
                self.shift_right_algebraic(i, s, wide_amount);
            }
            O::Srawi => {
                let s = self.gpr(rd);
                let wide_amount = self.b.ins().iconst(I64, i64::from(i.sh()));
                self.shift_right_algebraic(i, s, wide_amount);
            }
            O::Cntlzw => {
                let s = self.gpr(rd);
                let value = self.b.ins().clz(s);
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Extsb => {
                let s = self.gpr(rd);
                let byte = self.b.ins().ireduce(I8, s);
                let value = self.b.ins().sextend(I32, byte);
                self.set_gpr(ra, value);
                self.record(i, value);
            }
            O::Extsh => {
                let s = self.gpr(rd);
                let half = self.b.ins().ireduce(I16, s);
                let value = self.b.ins().sextend(I32, half);
                self.set_gpr(ra, value);
                self.record(i, value);
            }

            // ---- Branches ----
            O::B => {
                let target = if i.aa() { i.li() as u32 } else { pc.wrapping_add(i.li() as u32) };
                let known = target;
                let target = self.c32(target);
                return self.branch(i, pc, target, Some(known), None);
            }
            O::Bc => {
                let target = if i.aa() { i.bd() as u32 } else { pc.wrapping_add(i.bd() as u32) };
                let condition = self.branch_condition(i, true);
                let known = target;
                let target = self.c32(target);
                return self.branch(i, pc, target, Some(known), condition);
            }
            O::Bclr => {
                let lr = self.lr();
                let target = self.b.ins().band_imm_s(lr, !3_i64);
                let condition = self.branch_condition(i, true);
                return self.branch(i, pc, target, None, condition);
            }
            O::Bcctr => {
                let ctr = self.ctr();
                let target = self.b.ins().band_imm_s(ctr, !3_i64);
                let condition = self.branch_condition(i, false);
                return self.branch(i, pc, target, None, condition);
            }

            // ---- Loads and stores ----
            O::Lwz | O::Lwzu | O::Lhz | O::Lhzu | O::Lha | O::Lhau | O::Lbz | O::Lbzu => {
                let update = matches!(op, O::Lwzu | O::Lhzu | O::Lhau | O::Lbzu);
                let ea = self.ea_d(i, update);
                let size = match op {
                    O::Lwz | O::Lwzu => 4,
                    O::Lhz | O::Lhzu | O::Lha | O::Lhau => 2,
                    _ => 1,
                };
                self.integer_load(i, ea, size, matches!(op, O::Lha | O::Lhau), update);
            }
            O::Lwzx | O::Lwzux | O::Lhzx | O::Lhzux | O::Lhax | O::Lhaux | O::Lbzx | O::Lbzux => {
                let update = matches!(op, O::Lwzux | O::Lhzux | O::Lhaux | O::Lbzux);
                let ea = self.ea_x(i, update);
                let size = match op {
                    O::Lwzx | O::Lwzux => 4,
                    O::Lhzx | O::Lhzux | O::Lhax | O::Lhaux => 2,
                    _ => 1,
                };
                self.integer_load(i, ea, size, matches!(op, O::Lhax | O::Lhaux), update);
            }
            O::Stw | O::Stwu | O::Sth | O::Sthu | O::Stb | O::Stbu => {
                let update = matches!(op, O::Stwu | O::Sthu | O::Stbu);
                let ea = self.ea_d(i, update);
                let size = match op {
                    O::Stw | O::Stwu => 4,
                    O::Sth | O::Sthu => 2,
                    _ => 1,
                };
                self.integer_store(i, ea, size, update);
            }
            O::Stwx | O::Stwux | O::Sthx | O::Sthux | O::Stbx | O::Stbux => {
                let update = matches!(op, O::Stwux | O::Sthux | O::Stbux);
                let ea = self.ea_x(i, update);
                let size = match op {
                    O::Stwx | O::Stwux => 4,
                    O::Sthx | O::Sthux => 2,
                    _ => 1,
                };
                self.integer_store(i, ea, size, update);
            }
            O::Lwbrx => {
                let ea = self.ea_x(i, false);
                let address = self.host_address(ea);
                let value = self.b.ins().load(I32, flags(), address, 0);
                self.set_gpr(rd, value);
            }
            O::Lhbrx => {
                let ea = self.ea_x(i, false);
                let address = self.host_address(ea);
                let value = self.b.ins().uload16(I32, flags(), address, 0);
                self.set_gpr(rd, value);
            }
            O::Stwbrx => {
                let ea = self.ea_x(i, false);
                let value = self.gpr(rd);
                let address = self.host_address(ea);
                self.b.ins().store(flags(), value, address, 0);
            }
            O::Sthbrx => {
                let ea = self.ea_x(i, false);
                let value = self.gpr(rd);
                let half = self.b.ins().ireduce(I16, value);
                let address = self.host_address(ea);
                self.b.ins().store(flags(), half, address, 0);
            }
            O::Lmw => {
                let base = self.ea_d(i, false);
                for (n, reg) in (rd..32).enumerate() {
                    let ea = self.b.ins().iadd_imm_s(base, 4 * n as i64);
                    let value = self.load32(ea);
                    self.set_gpr(reg, value);
                }
            }
            O::Stmw => {
                let base = self.ea_d(i, false);
                for (n, reg) in (rd..32).enumerate() {
                    let ea = self.b.ins().iadd_imm_s(base, 4 * n as i64);
                    let value = self.gpr(reg);
                    self.store32(ea, value);
                }
            }
            O::Lfs | O::Lfsu => {
                let update = op == O::Lfsu;
                let ea = self.ea_d(i, update);
                self.load_single(i, ea, update);
            }
            O::Lfsx | O::Lfsux => {
                let update = op == O::Lfsux;
                let ea = self.ea_x(i, update);
                self.load_single(i, ea, update);
            }
            O::Lfd | O::Lfdu => {
                let update = op == O::Lfdu;
                let ea = self.ea_d(i, update);
                self.load_double(i, ea, update);
            }
            O::Lfdx | O::Lfdux => {
                let update = op == O::Lfdux;
                let ea = self.ea_x(i, update);
                self.load_double(i, ea, update);
            }
            O::Stfs | O::Stfsu => {
                let update = op == O::Stfsu;
                let ea = self.ea_d(i, update);
                self.store_single(i, ea, update);
            }
            O::Stfsx | O::Stfsux => {
                let update = op == O::Stfsux;
                let ea = self.ea_x(i, update);
                self.store_single(i, ea, update);
            }
            O::Stfd | O::Stfdu => {
                let update = op == O::Stfdu;
                let ea = self.ea_d(i, update);
                self.store_double(i, ea, update);
            }
            O::Stfdx | O::Stfdux => {
                let update = op == O::Stfdux;
                let ea = self.ea_x(i, update);
                self.store_double(i, ea, update);
            }

            // ---- Floating point (record forms use the interpreter) ----
            O::Fadds
            | O::Fsubs
            | O::Fmuls
            | O::Fdivs
            | O::Fmadds
            | O::Fmsubs
            | O::Fnmadds
            | O::Fnmsubs
                if !i.rc() =>
            {
                if let Some(value) = self.arith(op, i, 0) {
                    self.set_single(i, value);
                }
            }
            O::Fadd | O::Fsub | O::Fmul | O::Fdiv | O::Fmadd | O::Fmsub | O::Fnmadd | O::Fnmsub
                if !i.rc() =>
            {
                if let Some(value) = self.arith(op, i, 0) {
                    self.set_fpr(rd, 0, value);
                }
            }
            O::Frsp if !i.rc() => {
                let b = self.fpr(rb, 0);
                self.set_single(i, b);
            }
            O::Fmr | O::Fneg | O::Fabs | O::Fnabs if !i.rc() => {
                let b = self.fpr(rb, 0);
                let value = self.sign_op(op, b);
                self.set_fpr(rd, 0, value);
            }
            O::Fcmpu | O::Fcmpo => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 0);
                self.float_compare(i.crfd(), a, b);
            }

            O::Fctiwz if !i.rc() => {
                let b = self.fpr(rb, 0);
                let converted = self.b.ins().fcvt_to_sint_sat(I32, b);
                let nan = self.b.ins().fcmp(FloatCC::Unordered, b, b);
                let invalid = self.c32(0x8000_0000);
                let value = self.b.ins().select(nan, invalid, converted);
                let wide = self.b.ins().uextend(I64, value);
                let upper = self.b.ins().iconst(I64, 0xFFF8_0000_0000_0000_u64 as i64);
                let bits = self.b.ins().bor(wide, upper);
                let result = self.b.ins().bitcast(F64, MemFlagsData::new(), bits);
                self.set_fpr(rd, 0, result);
            }
            O::Stfiwx => {
                let ea = self.ea_x(i, false);
                let value = self.fpr(rd, 0);
                let bits = self.b.ins().bitcast(I64, MemFlagsData::new(), value);
                let low = self.b.ins().ireduce(I32, bits);
                self.store32(ea, low);
            }
            O::Fsel if !i.rc() => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 0);
                let c = self.fpr(i.rc_reg(), 0);
                let zero = self.b.ins().f64const(0.0);
                let positive = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, a, zero);
                let value = self.b.ins().select(positive, c, b);
                self.set_fpr(rd, 0, value);
            }

            // ---- Paired singles ----
            O::PsqL | O::PsqLu | O::PsqLx | O::PsqLux => {
                let indexed = matches!(op, O::PsqLx | O::PsqLux);
                let (gqr, single) =
                    if indexed { (i.psqx_i(), i.psqx_w()) } else { (i.psq_i(), i.psq_w()) };
                // Bit 2 of the load type: the integer forms.
                self.leave_unless_float(pc, gqr, 0x4_0000);
                let (ea, update) = self.psq_ea(i, op);
                let raw = self.load32(ea);
                let first = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
                let ps0 = self.b.ins().fpromote(F64, first);
                let ps1 = if single {
                    self.b.ins().f64const(1.0)
                } else {
                    let at = self.b.ins().iadd_imm_s(ea, 4);
                    let raw = self.load32(at);
                    let second = self.b.ins().bitcast(F32, MemFlagsData::new(), raw);
                    self.b.ins().fpromote(F64, second)
                };
                self.set_fpr(rd, 0, ps0);
                self.set_fpr(rd, 1, ps1);
                if update {
                    self.set_gpr(ra, ea);
                }
            }
            O::PsqSt | O::PsqStu | O::PsqStx | O::PsqStux => {
                let indexed = matches!(op, O::PsqStx | O::PsqStux);
                let (gqr, single) =
                    if indexed { (i.psqx_i(), i.psqx_w()) } else { (i.psq_i(), i.psq_w()) };
                // Bit 2 of the store type: the integer forms.
                self.leave_unless_float(pc, gqr, 0x4);
                let (ea, update) = self.psq_ea(i, op);
                let slots: &[(usize, i64)] = if single { &[(0, 0)] } else { &[(0, 0), (1, 4)] };
                for &(slot, offset) in slots {
                    let value = self.fpr(rd, slot);
                    let narrow = self.b.ins().fdemote(F32, value);
                    let raw = self.b.ins().bitcast(I32, MemFlagsData::new(), narrow);
                    let at = self.b.ins().iadd_imm_s(ea, offset);
                    self.store32(at, raw);
                }
                if update {
                    self.set_gpr(ra, ea);
                }
            }
            O::PsAdd
            | O::PsSub
            | O::PsMul
            | O::PsDiv
            | O::PsMadd
            | O::PsMsub
            | O::PsNmadd
            | O::PsNmsub
                if !i.rc() =>
            {
                if let (Some(ps0), Some(ps1)) = (self.arith(op, i, 0), self.arith(op, i, 1)) {
                    self.set_paired(i, ps0, ps1);
                }
            }
            O::PsMuls0 if !i.rc() => self.scaled(i, 0, false),
            O::PsMuls1 if !i.rc() => self.scaled(i, 1, false),
            O::PsMadds0 if !i.rc() => self.scaled(i, 0, true),
            O::PsMadds1 if !i.rc() => self.scaled(i, 1, true),
            O::PsSum0 if !i.rc() => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 1);
                let c = self.fpr(i.rc_reg(), 1);
                let sum = self.b.ins().fadd(a, b);
                self.set_paired(i, sum, c);
            }
            O::PsSum1 if !i.rc() => {
                let a = self.fpr(ra, 0);
                let b = self.fpr(rb, 1);
                let c = self.fpr(i.rc_reg(), 0);
                let sum = self.b.ins().fadd(a, b);
                self.set_paired(i, c, sum);
            }
            O::PsMerge00 | O::PsMerge01 | O::PsMerge10 | O::PsMerge11 if !i.rc() => {
                let first = self.fpr(ra, usize::from(matches!(op, O::PsMerge10 | O::PsMerge11)));
                let second = self.fpr(rb, usize::from(matches!(op, O::PsMerge01 | O::PsMerge11)));
                self.set_fpr(rd, 0, first);
                self.set_fpr(rd, 1, second);
            }
            O::PsMr | O::PsNeg | O::PsAbs | O::PsNabs if !i.rc() => {
                let ps0 = self.fpr(rb, 0);
                let ps1 = self.fpr(rb, 1);
                let ps0 = self.sign_op(op, ps0);
                let ps1 = self.sign_op(op, ps1);
                self.set_fpr(rd, 0, ps0);
                self.set_fpr(rd, 1, ps1);
            }

            // ---- System ----
            O::Mfspr if i.spr() == spr::LR => {
                let value = self.lr();
                self.set_gpr(rd, value);
            }
            O::Mfspr if i.spr() == spr::CTR => {
                let value = self.ctr();
                self.set_gpr(rd, value);
            }
            O::Mtspr if i.spr() == spr::LR => {
                let value = self.gpr(rd);
                self.set_lr(value);
            }
            O::Mtspr if i.spr() == spr::CTR => {
                let value = self.gpr(rd);
                self.set_ctr(value);
            }
            O::Mfcr => {
                let value = self.cr();
                self.set_gpr(rd, value);
            }
            O::Mtcrf => {
                let mask = (0..8)
                    .filter(|field| i.crm() & (0x80 >> field) != 0)
                    .fold(0_u32, |mask, field| mask | (0xF000_0000 >> (4 * field)));
                let cr = self.cr();
                let s = self.gpr(rd);
                let kept = self.b.ins().band_imm_s(cr, i64::from(!mask as i32));
                let taken = self.b.ins().band_imm_s(s, i64::from(mask as i32));
                let value = self.b.ins().bor(kept, taken);
                self.set_cr(value);
            }
            O::Crand
            | O::Crandc
            | O::Creqv
            | O::Crnand
            | O::Crnor
            | O::Cror
            | O::Crorc
            | O::Crxor => {
                let a = self.cr_bit(ra);
                let b = self.cr_bit(rb);
                let value = match op {
                    O::Crand => self.b.ins().band(a, b),
                    O::Crandc => {
                        let inverse = self.b.ins().bxor_imm_s(b, 1);
                        self.b.ins().band(a, inverse)
                    }
                    O::Creqv => {
                        let x = self.b.ins().bxor(a, b);
                        self.b.ins().bxor_imm_s(x, 1)
                    }
                    O::Crnand => {
                        let x = self.b.ins().band(a, b);
                        self.b.ins().bxor_imm_s(x, 1)
                    }
                    O::Crnor => {
                        let x = self.b.ins().bor(a, b);
                        self.b.ins().bxor_imm_s(x, 1)
                    }
                    O::Cror => self.b.ins().bor(a, b),
                    O::Crorc => {
                        let inverse = self.b.ins().bxor_imm_s(b, 1);
                        self.b.ins().bor(a, inverse)
                    }
                    _ => self.b.ins().bxor(a, b),
                };
                let shift = 31 - rd as i64;
                let cr = self.cr();
                let cleared = self.b.ins().band_imm_s(cr, !(1_i64 << shift));
                let placed = self.b.ins().ishl_imm_s(value, shift);
                let merged = self.b.ins().bor(cleared, placed);
                self.set_cr(merged);
            }
            O::Sync
            | O::Isync
            | O::Eieio
            | O::Dcbf
            | O::Dcbst
            | O::Dcbt
            | O::Dcbtst
            | O::Dcbi
            | O::Icbi => {}

            _ => return Flow::Fallback,
        }
        Flow::Next
    }

    fn shift_right_algebraic(&mut self, i: Instr, s: Value, amount: Value) {
        let wide = self.b.ins().sextend(I64, s);
        let shifted = self.b.ins().sshr(wide, amount);
        let value = self.b.ins().ireduce(I32, shifted);
        // CA: negative source and one bits shifted out.
        let back = self.b.ins().ishl(shifted, amount);
        let lost = self.b.ins().icmp(IntCC::NotEqual, back, wide);
        let negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, s, 0);
        let carry = self.b.ins().band(lost, negative);
        let carry = self.b.ins().uextend(I32, carry);
        self.set_ca(carry);
        self.set_gpr(i.ra(), value);
        self.record(i, value);
    }
}

/// Instructions after which the rest of the block must not run natively:
/// the interpreter may have redirected `pc`.
const fn ends_block_after_fallback(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::HleCall | Opcode::Sc | Opcode::Rfi | Opcode::Invalid | Opcode::Tw | Opcode::Twi
    )
}

/// A block to translate, decoded where the guest's memory can be read.
#[derive(Clone)]
pub(super) struct Job {
    pub(super) pc: u32,
    pub(super) instructions: Vec<(u32, Opcode, Instr)>,
    /// Address after the last instruction.
    next: u32,
    /// `interpret_one` for the bus in use.
    helper: usize,
    generation: u64,
    /// Worth the time to make it run as fast as it can.
    optimized: bool,
    /// Where the quick translation counts its runs (0: it does not count).
    counter: usize,
}

impl Job {
    /// The result of a translation that went wrong.
    pub(super) const fn failed(&self) -> Translated {
        Translated {
            pc: self.pc,
            code: None,
            alignment: 1,
            generation: self.generation,
            nanos: 0,
            optimized: self.optimized,
        }
    }

    /// The same block again, to be made as fast as it can run.
    pub(super) fn into_optimized(self) -> Self {
        Self { optimized: true, counter: 0, ..self }
    }

    /// Where the quick translation counts its runs.
    pub(super) fn counter_address(&self) -> *const u32 {
        self.counter as *const u32
    }

    /// Has the quick translation count its runs at `counter`, a `u32` that outlives the code.
    pub(super) fn count_runs_at(&mut self, counter: *mut u32) {
        self.counter = counter as usize;
    }

    /// Whether the block has code in `[start, end)`.
    pub(super) const fn overlaps(&self, start: u32, end: u32) -> bool {
        self.pc < end && start < self.next
    }

    /// A translation of it is good for the code as of `generation`.
    pub(super) const fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }
}

/// What became of a [`Job`].
pub(super) struct Translated {
    pub(super) pc: u32,
    /// Machine code without relocations, `None` if translating failed.
    pub(super) code: Option<Vec<u8>>,
    pub(super) alignment: u64,
    pub(super) generation: u64,
    pub(super) nanos: u64,
    /// Comes from an [optimizing job](Job::into_optimized).
    pub(super) optimized: bool,
}

/// Whether nothing after `op` can run: it always branches away.
const fn ends_block(op: Opcode, i: Instr) -> bool {
    let bo = i.rd();
    match op {
        Opcode::B => true,
        Opcode::Bc | Opcode::Bclr => bo & 0b1_0100 == 0b1_0100,
        Opcode::Bcctr => bo & 0b1_0000 != 0,
        _ => ends_block_after_fallback(op),
    }
}

/// Reads the instructions of the block at `pc`.
#[allow(clippy::fn_to_numeric_cast_any, reason = "native code calls it by its address")]
pub(super) fn decode_block<B: Bus>(pc: u32, bus: &mut B, generation: u64) -> Result<Job, CpuError> {
    // Decode first so an unreadable first instruction reports a clean error.
    let mut instructions = Vec::with_capacity(16);
    let mut at = pc;
    while instructions.len() < MAX_BLOCK_INSTRUCTIONS as usize {
        let Ok(raw) = bus.fetch(at) else {
            if instructions.is_empty() {
                return Err(CpuError::Bus { pc, address: at });
            }
            break;
        };
        let op = decode(raw);
        instructions.push((at, op, Instr(raw)));
        at = at.wrapping_add(4);
        if ends_block(op, Instr(raw)) {
            break;
        }
    }
    let helper =
        interpret_one::<B> as unsafe extern "C" fn(*mut CpuState, *mut HelperContext) -> u32;
    Ok(Job {
        pc,
        instructions,
        next: at,
        helper: helper as usize,
        generation,
        optimized: false,
        counter: 0,
    })
}

/// One translation thread.
pub(super) struct Worker {
    /// Translates quickly.
    isa: OwnedTargetIsa,
    /// Makes faster code.
    optimizing_isa: OwnedTargetIsa,
    context: Context,
    builder_context: FunctionBuilderContext,
    pages: usize,
}

impl Worker {
    pub(super) fn new(isa: OwnedTargetIsa, optimizing_isa: OwnedTargetIsa, pages: usize) -> Self {
        Self {
            isa,
            optimizing_isa,
            context: Context::new(),
            builder_context: FunctionBuilderContext::new(),
            pages,
        }
    }

    pub(super) fn translate(&mut self, job: &Job) -> Translated {
        let timer = std::time::Instant::now();
        let code = self.compile(job);
        self.context.clear();
        let (code, alignment) = code.map_or((None, 1), |(code, alignment)| (Some(code), alignment));
        Translated {
            pc: job.pc,
            code,
            alignment,
            generation: job.generation,
            nanos: timer.elapsed().as_nanos() as u64,
            optimized: job.optimized,
        }
    }

    fn compile(&mut self, job: &Job) -> Option<(Vec<u8>, u64)> {
        let pointer = self.isa.pointer_type();
        let call_conv = self.isa.default_call_conv();
        self.context.func.signature = block_signature(pointer);
        {
            let mut b = FunctionBuilder::new(&mut self.context.func, &mut self.builder_context);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            let (state, memory, helper_context) =
                (b.block_params(entry)[0], b.block_params(entry)[1], b.block_params(entry)[2]);

            let mut signature = Signature::new(call_conv);
            signature.params.extend([AbiParam::new(pointer); 2]);
            signature.returns.push(AbiParam::new(I32));
            let interpret_sig = b.import_signature(signature);
            let block_sig = b.import_signature(block_signature(pointer));
            let interpret = b.ins().iconst(pointer, job.helper as i64);
            if job.counter != 0 {
                // Three instructions per run tell the hot blocks, the ones worth optimizing,
                // from the many that run a few times only.
                let counter = b.ins().iconst(pointer, job.counter as i64);
                let runs = b.ins().load(I32, flags(), counter, 0);
                let runs = b.ins().iadd_imm_u(runs, 1);
                b.ins().store(flags(), runs, counter, 0);
            }

            let mut emit = Emit {
                b: &mut b,
                state,
                memory,
                helper_context,
                interpret,
                interpret_sig,
                block_sig,
                pages: self.pages,
                executed: 0,
                gpr: [None; 32],
                gpr_dirty: 0,
                fpr: [[None; 2]; 32],
                fpr_dirty: [0; 2],
                lr: None,
                ctr: None,
                cr: None,
                ca: None,
                lr_dirty: false,
                ctr_dirty: false,
                cr_dirty: false,
                ca_dirty: false,
            };
            let mut ended = false;
            for &(address, op, i) in &job.instructions {
                emit.executed += 1;
                match emit.instruction(op, i, address) {
                    Flow::Next => {}
                    Flow::Ended => {
                        ended = true;
                        break;
                    }
                    Flow::Fallback => {
                        emit.interpret(address);
                        if ends_block_after_fallback(op) {
                            // `state.pc` is what the interpreter left there.
                            emit.charge();
                            emit.leave();
                            ended = true;
                            break;
                        }
                    }
                }
            }
            if !ended {
                let next = emit.c32(job.next);
                emit.end_block(next, Some(job.next));
            }
            b.finalize(self.isa.frontend_config());
        }
        let isa = if job.optimized { &self.optimizing_isa } else { &self.isa };
        let compiled = match self.context.compile(&**isa, &mut ControlPlane::default()) {
            Ok(compiled) => compiled,
            Err(error) => {
                log::error!("JIT failed at {:#010x}: {:?}", job.pc, error.inner);
                return None;
            }
        };
        if !compiled.buffer.relocs().is_empty() {
            log::error!("JIT block at {:#010x} needs relocations", job.pc);
            return None;
        }
        Some((compiled.code_buffer().to_vec(), u64::from(compiled.buffer.min_alignment)))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Bus, CpuState, Fpr, INSTR_BLR, StepEvent, encode_hle_call,
        interpreter::{asm::*, step, test_bus::TestBus},
        jit::Jit,
    };

    /// Bus with a flat, directly addressable memory (little-endian host view of big-endian bytes).
    struct FlatBus(TestBus);

    impl Bus for FlatBus {
        fn read_u8(&mut self, a: u32) -> Result<u8, crate::BusError> {
            self.0.read_u8(a)
        }
        fn read_u16(&mut self, a: u32) -> Result<u16, crate::BusError> {
            self.0.read_u16(a)
        }
        fn read_u32(&mut self, a: u32) -> Result<u32, crate::BusError> {
            self.0.read_u32(a)
        }
        fn read_u64(&mut self, a: u32) -> Result<u64, crate::BusError> {
            self.0.read_u64(a)
        }
        fn write_u8(&mut self, a: u32, v: u8) -> Result<(), crate::BusError> {
            self.0.write_u8(a, v)
        }
        fn write_u16(&mut self, a: u32, v: u16) -> Result<(), crate::BusError> {
            self.0.write_u16(a, v)
        }
        fn write_u32(&mut self, a: u32, v: u32) -> Result<(), crate::BusError> {
            self.0.write_u32(a, v)
        }
        fn write_u64(&mut self, a: u32, v: u64) -> Result<(), crate::BusError> {
            self.0.write_u64(a, v)
        }
        fn timebase(&mut self) -> u64 {
            self.0.timebase()
        }
        fn fast_memory(&mut self) -> Option<*mut u8> {
            Some(self.0.mem.as_mut_ptr())
        }
    }

    /// A program exercising many translated forms: ends in `hle_call 1`.
    fn program() -> Vec<u32> {
        let rlwinm = |rs: u32, ra: u32, sh: u32, mb: u32, me: u32| {
            (21 << 26) | (rs << 21) | (ra << 16) | (sh << 11) | (mb << 6) | (me << 1)
        };
        vec![
            li(3, 0x1234),
            d(15, 4, 3, 2),            // addis r4, r3, 2
            x(31, 3, 5, 4, 266, true), // add. r5, r3, r4
            d(36, 5, 0, 0x2000),       // stw r5, 0x2000(0)
            d(32, 6, 0, 0x2000),       // lwz r6, 0x2000(0)
            d(40, 7, 0, 0x2002),       // lhz r7, 0x2002(0)
            d(34, 8, 0, 0x2003),       // lbz r8, 0x2003(0)
            rlwinm(6, 9, 8, 16, 31),   // rlwinm r9, r6, 8, 16, 31
            li(10, -5),
            d(12, 11, 10, 7),             // addic r11, r10, 7 (carries)
            x(31, 3, 12, 10, 138, false), // adde r12, r3, r10
            d(11, 0, 10, 0),              // cmpwi r10, 0
            x(31, 10, 13, 2, 824, true),  // srawi. r13, r10, 2
            d(38, 8, 0, 0x2010),          // stb r8, 0x2010(0)
            li(14, 3),
            (31 << 26) | (14 << 21) | (9 << 16) | (467 << 1), // mtctr r14
            d(14, 15, 15, 1),      // loop: addi r15, r15, 1 (r15 starts 0, rA=15)
            0x4200_FFFC,           // bdnz loop
            d(48, 1, 0, 0x2020),   // lfs f1, 0x2020(0)
            a(63, 2, 1, 1, 0, 21), // fadd f2, f1, f1
            a(59, 3, 2, 0, 1, 25), // fmuls f3, f2, f1
            d(52, 3, 0, 0x2030),   // stfs f3, 0x2030(0)
            (4 << 26) | (4 << 21) | (1 << 16) | (2 << 11) | (21 << 1), // ps_add f4, f1, f2
            (63 << 26) | (5 << 21) | (2 << 11) | (40 << 1), // fneg f5, f2
            ((63 << 26) | (6 << 16) | (2 << 11)), // fcmpu cr0, f6, f2
            encode_hle_call(1),
            INSTR_BLR,
        ]
    }

    /// Float and integer quantization, conversions, and branches taken both ways
    /// inside one block. GQR1 quantizes to `s16`, which leaves the block.
    fn second_program() -> Vec<u32> {
        let psq = |op: u32, rt: u32, ra: u32, w: u32, gqr: u32, offset: u32| {
            (op << 26) | (rt << 21) | (ra << 16) | (w << 15) | (gqr << 12) | offset
        };
        vec![
            psq(56, 1, 0, 0, 0, 0x400), // psq_l f1, 0x400(0), 0, qr0
            psq(56, 2, 0, 1, 0, 0x404), // psq_l f2, 0x404(0), 1, qr0
            li(5, 0x400),
            psq(57, 3, 5, 0, 0, 8),     // psq_lu f3, 8(r5), 0, qr0
            psq(60, 1, 0, 0, 0, 0x410), // psq_st f1, 0x410(0), 0, qr0
            psq(60, 2, 0, 1, 0, 0x418), // psq_st f2, 0x418(0), 1, qr0
            psq(56, 4, 0, 0, 1, 0x420), // psq_l f4, 0x420(0), 0, qr1
            psq(60, 4, 0, 0, 1, 0x428), // psq_st f4, 0x428(0), 0, qr1
            (63 << 26) | (6 << 21) | (3 << 11) | (15 << 1), // fctiwz f6, f3
            li(7, 0x430),
            x(31, 6, 0, 7, 983, false), // stfiwx f6, 0, r7
            a(63, 8, 1, 2, 3, 23),      // fsel f8, f1, f3, f2
            li(9, 5),
            d(11, 0, 9, 5),                                   // cmpwi r9, 5
            0x4182_0008,                                      // beq +8 (taken)
            li(10, 111),                                      // skipped
            d(11, 0, 9, 6),                                   // cmpwi r9, 6
            0x4182_0008,                                      // beq +8 (not taken)
            li(11, 222),                                      // runs
            0x4800_0005,                                      // bl +4
            (31 << 26) | (12 << 21) | (8 << 16) | (339 << 1), // mflr r12
            encode_hle_call(2),
            INSTR_BLR,
        ]
    }

    /// Runs `program` both ways and compares everything observable.
    fn assert_same_as_interpreter(program: &[u32], set: impl Fn(&mut TestBus, &mut CpuState)) {
        let prepare = || {
            let mut bus = TestBus::new();
            bus.load_program(0x100, program);
            let mut state = CpuState { pc: 0x100, gqr: [0; 8], ..CpuState::default() };
            set(&mut bus, &mut state);
            (bus, state)
        };
        let (mut reference_bus, mut reference) = prepare();
        let event = loop {
            let event = step(&mut reference, &mut reference_bus).unwrap();
            if event != StepEvent::Continue {
                break event;
            }
        };
        for synchronous in [true, false] {
            let (bus, mut state) = prepare();
            let mut bus = FlatBus(bus);
            let mut jit = Jit::new();
            jit.set_synchronous(synchronous);
            let (jit_event, executed) = jit.run(&mut state, &mut bus, 1000).unwrap();
            assert_eq!(jit_event, event);
            assert!(executed > 0);
            assert!(!synchronous || jit.compiled_blocks() > 0);
            assert_eq!(state.gpr, reference.gpr);
            for (index, (got, want)) in state.fpr.iter().zip(&reference.fpr).enumerate() {
                assert!(
                    got.ps0.to_bits() == want.ps0.to_bits()
                        && got.ps1.to_bits() == want.ps1.to_bits(),
                    "f{index}: {got:?} vs {want:?}"
                );
            }
            assert_eq!(
                (state.pc, state.lr, state.ctr, state.cr),
                (reference.pc, reference.lr, reference.ctr, reference.cr)
            );
            assert_eq!(bus.0.mem[0x400..0x440], reference_bus.mem[0x400..0x440]);
        }
    }

    #[test]
    fn quantized_accesses_and_inner_branches_match_the_interpreter() {
        assert_same_as_interpreter(&second_program(), |bus, state| {
            state.gqr[1] = 0x0007_0007;
            bus.load_program(
                0x400,
                &[
                    1.5_f32.to_bits(),
                    (-2.25_f32).to_bits(),
                    70_000.75_f32.to_bits(),
                    2.5_f32.to_bits(),
                ],
            );
            bus.load_program(0x420, &[0x1234_FFFE]);
        });
    }

    #[test]
    fn a_used_up_budget_stops_a_chain_of_blocks() {
        // b +4 forever.
        let (mut bus, mut state) = (TestBus::new(), CpuState { pc: 0x100, ..CpuState::default() });
        bus.load_program(0x100, &[0x4800_0000]);
        let mut bus = FlatBus(bus);
        let mut jit = Jit::new();
        jit.set_synchronous(true);
        let (event, executed) = jit.run(&mut state, &mut bus, 100).unwrap();
        assert_eq!(event, StepEvent::Continue);
        assert_eq!(executed, 100);
        assert_eq!(jit.compiled_blocks(), 1);
    }

    fn setup() -> (TestBus, CpuState) {
        let mut bus = TestBus::new();
        bus.load_program(0x100, &program());
        bus.load_program(0x2020, &[1.5_f32.to_bits()]);
        (bus, CpuState { pc: 0x100, gqr: [0; 8], ..CpuState::default() })
    }

    /// The JIT must leave exactly the state the reference interpreter leaves.
    #[test]
    fn matches_the_interpreter() {
        let (mut reference_bus, mut reference) = setup();
        let event = loop {
            let event = step(&mut reference, &mut reference_bus).unwrap();
            if event != StepEvent::Continue {
                break event;
            }
        };
        assert_eq!(event, StepEvent::HleCall(1));

        let (bus, mut state) = setup();
        let mut bus = FlatBus(bus);
        let mut jit = Jit::new();
        jit.set_synchronous(true);
        let (jit_event, _) = jit.run(&mut state, &mut bus, 1000).unwrap();
        assert_eq!(jit_event, event);
        assert!(jit.compiled_blocks() > 0, "the blocks must really be translated");
        assert_eq!(state.gpr, reference.gpr);
        for (index, (got, want)) in state.fpr.iter().zip(&reference.fpr).enumerate() {
            let same = |a: f64, b: f64| a.to_bits() == b.to_bits();
            assert!(
                same(got.ps0, want.ps0) && same(got.ps1, want.ps1),
                "f{index}: {got:?} vs {want:?}"
            );
        }
        assert_eq!(
            (state.pc, state.lr, state.ctr, state.cr),
            (reference.pc, reference.lr, reference.ctr, reference.cr)
        );
        assert_eq!(
            (state.xer_ca, state.xer_so, state.fpscr),
            (reference.xer_ca, reference.xer_so, reference.fpscr)
        );
        assert_eq!(bus.0.mem[0x2000..0x2040], reference_bus.mem[0x2000..0x2040]);
        assert!(state.fpr[3] != Fpr::default());
    }
}
