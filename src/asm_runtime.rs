#![allow(unused_macros)]

//! Assembly runtime and support functions for the GBA.

// Note(Lokathor): Functions here will *definitely* panic without the `on_gba`
// cargo feature enabled, and so they should all have the `track_caller`
// attribute set whenever the `on_gba` feature is *disabled*

use crate::gba_cell::GbaCell;

use bracer::*;

/// Inserts a `nop` instruction.
#[inline(always)]
#[cfg_attr(not(feature = "on_gba"), track_caller)]
pub fn nop() {
  on_gba_or_unimplemented! {
    unsafe {
      core::arch::asm! {
        "nop",
      }
    }
  }
}

/// Atomically swap `x` and the 32-bit value stored at `ptr`.
///
/// ## Safety
/// This both reads and writes `ptr`, so all the usual rules of that apply.
#[inline]
#[cfg_attr(feature = "on_gba", instruction_set(arm::a32))]
#[cfg_attr(not(feature = "on_gba"), track_caller)]
pub unsafe fn swp(mut ptr: *mut u32, x: u32) -> u32 {
  on_gba_or_unimplemented! {
    let output: u32;
    // Note(Lokathor): This won't actually alter the pointer register, but we
    // *tell* LLVM that it will because the pointer register can't be used as
    // the output of the swapping operation.
    #[allow(unused_assignments)]
    unsafe {
      core::arch::asm! {
        "swp {output}, {input}, [{addr}]",
        output = lateout(reg) output,
        input = in(reg) x,
        addr = inlateout(reg) ptr,
      }
    }
    output
  }
}

/// Atomically swap `x` and the 8-bit value stored at `ptr`.
///
/// ## Safety
/// This both reads and writes `ptr`, so all the usual rules of that apply.
#[inline]
#[cfg_attr(feature = "on_gba", instruction_set(arm::a32))]
#[cfg_attr(not(feature = "on_gba"), track_caller)]
pub unsafe fn swpb(mut ptr: *mut u8, x: u8) -> u8 {
  on_gba_or_unimplemented! {
    let output: u8;
    // Note(Lokathor): This won't actually alter the pointer register, but we
    // *tell* LLVM that it will because the pointer register can't be used as
    // the output of the swapping operation.
    #[allow(unused_assignments)]
    unsafe {
      core::arch::asm! {
        "swpb {output}, {input}, [{addr}]",
        output = lateout(reg) output,
        input = in(reg) x,
        addr = inlateout(reg) ptr,
      }
    }
    output
  }
}

// Proc-macros can't see the target being built for, so we use this declarative
// macro to determine if we're on a thumb target (and need to force our asm into
// a32 mode) or if we're not on thumb (and our asm can pass through untouched).
#[cfg(target_feature = "thumb-mode")]
macro_rules! force_a32 {
  ($($asm_line:expr),+ $(,)?) => {
    t32_with_a32_scope! {
      $( concat!($asm_line, "\n") ),+ ,
    }
  }
}
#[cfg(not(target_feature = "thumb-mode"))]
macro_rules! force_a32 {
  ($($asm_line:expr),+ $(,)?) => {
    concat!(
      $( concat!($asm_line, "\n") ),+ ,
    )
  }
}

#[cfg(feature = "on_gba")]
core::arch::global_asm! {
  put_fn_in_section!(".text._start"),
  ".global _start",
  force_a32!{
    "_start:",
    // space for the rom header to be placed after compilation.
    "b 1f",
    ".space 0xE0",
    "1:",

    "mov r12, #0x04000000",
    "add r3, r12, #0xD4",

    // Now
    // * r12: mmio base
    // * r3: dma3 base

    // Configure WAITCNT to the GBATEK suggested default
    "add r0, r12, #0x204",
    "ldr r1, =0x4317",
    "strh r1, [r0]",

    // activate mGBA logging, when available
    "ldr r0, =0x04FFF780",
    "ldr r1, =0xC0DE",
    "strh r1, [r0]",

    /* iwram copy */
    "ldr r0, =_iwram_word_copy_count",
    when!(("r0" != "#0")[1] {
      "ldr r1, =_iwram_position_in_rom",
      "str r1, [r3]",           // src
      "ldr r1, =_iwram_start",
      "str r1, [r3, #4]",       // dest
      "strh r0, [r3, #8]",      // transfers
      "mov r1, #(1<<10|1<<15)", // 32-bit transfers, enable
      "strh r1, [r3, #10]",
    }),

    /* ewram copy */
    "ldr r4, =_ewram_word_copy_count",
    when!(("r4" != "#0")[1] {
      "ldr r1, =_ewram_position_in_rom",
      "str r1, [r3]",
      "ldr r1, =_ewram_start",
      "str r1, [r3, #4]",
      "strh r0, [r3, #8]",
      "mov r1, #(1<<10|1<<15)",
      "strh r1, [r3, #10]",
    }),

    /* bss zero */
    "ldr r4, =_bss_word_clear_count",
    when!(("r4" != "#0")[1] {
      "ldr r0, =_bss_start",
      "mov r2, #0",
      "2:",
      "str r2, [r0], #4",
      "subs r4, r4, #1",
      "bne 2b",
    }),

    // Tell the BIOS about our irq handler
    "ldr r0, =_asm_runtime_irq_handler",
    "str r0, [r12, #-4]",

    // Note(Lokathor): we do a `bx` instead of a `b` because it saves 4 *entire*
    // bytes (!), since `main` will usually be a t32 function and thus usually
    // requires a linker shim to call.
    "ldr r0, =main",
    "bx r0",

    // TODO: should we soft reset or something if `main` returns?
  }
}

// This "default" IRQ handler
// * Assumes that `IntrWait` support is desired.
// * Calls the user handler function in System mode.
// * Does NOT support nested interrupts at all.
#[cfg(feature = "on_gba")]
core::arch::global_asm! {
  put_fn_in_section!(".iwram.text._asm_runtime_irq_handler"),
  ".global _asm_runtime_irq_handler",
  force_a32!{
    "_asm_runtime_irq_handler:",
    // We're allowed to use the usual C ABI registers.

    /* A fox wizard told me how to do this one */
    // handle MMIO interrupt system
    "mov  r12, 0x04000000",     // load r12 with a 1 cycle value
    "ldr  r0, [r12, #0x200]!",  // load IE_IF with r12 writeback
    "and  r0, r0, r0, LSR #16", // bits = IE & IF
    "strh r0, [r12, #2]",       // write16 to just IF
    // handle BIOS IntrWait system
    "ldr  r1, [r12, #-0x208]!", // load BIOS_IF_?? with r12 writeback
    "orr  r1, r1, r0",          // mark `bits` as `has_occurred`
    "strh r1, [r12]",           // write16 to just BIOS_IF

    // Get the user handler fn pointer, call it if non-null.
    "ldr r12, ={USER_IRQ_HANDLER}",
    "ldr r12, [r12]",
    when!(("r12" != "#0")[1] {
      a32_read_spsr_to!("r3"),
      "push {{r3, lr}}",
      a32_set_cpu_control!(System, irq_masked = true, fiq_masked = true),
      a32_fake_blx!("r12"),
      a32_set_cpu_control!(IRQ, irq_masked = true, fiq_masked = true),
      "pop {{r3, lr}}",
      a32_write_spsr_from!("r3"),
    }),

    // return to the BIOS
    "bx lr",
  },
  USER_IRQ_HANDLER = sym USER_IRQ_HANDLER,
}

/// The user-provided interrupt request handler function.
#[cfg(feature = "on_gba")]
pub static USER_IRQ_HANDLER: GbaCell<
  Option<unsafe extern "C" fn(crate::irq::IrqBits)>,
> = GbaCell::new(None);
