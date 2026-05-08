// Integration tests for `Cpu::step_over` and `Cpu::step_out`.
//
// These exercise the user-facing "next-line" / "finish-function"
// behaviour: skip over a call instead of descending into it, or run
// until the current callee returns. Tests stay at the `Cpu + Memory`
// level so the call/return decisions and the breakpoint short-circuit
// are checked without any GUI in the way.

use em6809_core::bus::Memory;
use em6809_core::cpu::{Cpu, StepStop};

fn boot(_mem: &Memory) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.r.pc = 0x1000;
    cpu.r.s = 0x2000;
    cpu
}

// ============================================================
// step_over
// ============================================================

#[test]
fn step_over_bsr_lands_on_byte_after_call() {
    // $1000  8D 02   BSR  +2 -> $1004
    // $1002  12      NOP                (return target)
    // $1003  39      RTS                (caller's RTS, never reached here)
    // $1004  39      RTS                (callee body: just return)
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x39]);
    let mut cpu = boot(&mem);

    let stop = cpu.step_over(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1002, "should land on the NOP after BSR");
    assert!(
        cpu.shadow_stack.is_empty(),
        "BSR's CALL frame should have been popped by RTS"
    );
}

#[test]
fn step_over_jsr_extended_uses_disasm_length() {
    // $1000  BD 12 34  JSR $1234   (3-byte instruction)
    // $1003  12        NOP         (return target)
    // $1234  39        RTS
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0xBD, 0x12, 0x34, 0x12]);
    mem.load_slice(0x1234, &[0x39]);
    let mut cpu = boot(&mem);

    let stop = cpu.step_over(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1003);
}

#[test]
fn step_over_swi_uses_vector_then_rti() {
    // $1000  3F        SWI
    // $1001  12        NOP   (return target after SWI)
    // $4000  3B        RTI   (handler: just return)
    // $FFFA/$FFFB -> $4000
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x3F, 0x12]);
    mem.load_slice(0x4000, &[0x3B]);
    mem.load_slice(0xFFFA, &[0x40, 0x00]);
    let mut cpu = boot(&mem);

    let stop = cpu.step_over(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1001, "should land just past the SWI");
}

#[test]
fn step_over_non_call_acts_like_plain_step() {
    // $1000  12   NOP   (not a call)
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x12]);
    let mut cpu = boot(&mem);

    let stop = cpu.step_over(&mut mem, 1000);
    assert_eq!(stop, StepStop::NotACall);
    assert_eq!(cpu.r.pc, 0x1001);
}

#[test]
fn step_over_breakpoint_inside_callee_short_circuits() {
    // $1000  8D 02   BSR  +2 -> $1004
    // $1002  12      NOP        (return target)
    // $1004  12      NOP        (callee body)
    // $1005  39      RTS
    //
    // BP at $1004 (the very first instruction inside the callee) so
    // step_over halts before reaching $1002.
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x12, 0x39]);
    let mut cpu = boot(&mem);
    let id = cpu.breakpoints.add(0x1004);

    let stop = cpu.step_over(&mut mem, 1000);
    assert_eq!(stop, StepStop::Breakpoint(id));
    assert_eq!(cpu.r.pc, 0x1004);
}

#[test]
fn step_over_runaway_callee_hits_limit() {
    // Callee never returns; step_over must give up after `limit`.
    // $1000  8D 02   BSR  +2 -> $1004
    // $1002  12      NOP        (never reached)
    // $1004  20 FE   BRA  -2    (infinite loop at $1004)
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x20, 0xFE]);
    let mut cpu = boot(&mem);

    let stop = cpu.step_over(&mut mem, 50);
    assert_eq!(stop, StepStop::Limit);
}

#[test]
fn step_over_swi2_two_byte_prefix_handled() {
    // SWI2 is $10 $3F. Confirm step_over returns to byte after the
    // 2-byte instruction (i.e., $1002), not $1001.
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x10, 0x3F, 0x12]);
    mem.load_slice(0x4000, &[0x3B]); // RTI
    mem.load_slice(0xFFF4, &[0x40, 0x00]); // SWI2 vector
    let mut cpu = boot(&mem);

    let stop = cpu.step_over(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1002);
}

// ============================================================
// step_out
// ============================================================

#[test]
fn step_out_runs_until_callee_returns() {
    // $1000  8D 02   BSR  +2 -> $1004     (caller)
    // $1002  12      NOP                  (return target)
    // $1004  12      NOP                  (callee body)
    // $1005  12      NOP
    // $1006  39      RTS
    //
    // Step into the callee first, then call step_out — should land
    // on $1002.
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x12, 0x12, 0x39]);
    let mut cpu = boot(&mem);

    cpu.step(&mut mem, false); // BSR -> at $1004 with one frame on shadow stack
    assert_eq!(cpu.r.pc, 0x1004);
    assert_eq!(cpu.shadow_stack.len(), 1);

    let stop = cpu.step_out(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1002);
    assert!(cpu.shadow_stack.is_empty());
}

#[test]
fn step_out_empty_stack_returns_marker() {
    // No call has been made — shadow stack is empty.
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x12]);
    let mut cpu = boot(&mem);
    assert!(cpu.shadow_stack.is_empty());

    let stop = cpu.step_out(&mut mem, 1000);
    assert_eq!(stop, StepStop::EmptyStack);
    assert_eq!(cpu.r.pc, 0x1000, "PC should not advance");
}

#[test]
fn step_out_through_swi_handler_uses_shadow_frame() {
    // SWI handler: the S-stack top is the saved CC byte, NOT a return
    // PC, so a "read 2 bytes off S" approach would land somewhere
    // bogus. Using shadow_stack.peek().return_addr lands correctly
    // on $1001 (the byte after the SWI).
    //
    // $1000  3F        SWI
    // $1001  12        NOP            (return target)
    // $4000  3B        RTI            (handler body — never executed in this test)
    // $FFFA/$FFFB -> $4000
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x3F, 0x12]);
    mem.load_slice(0x4000, &[0x3B]);
    mem.load_slice(0xFFFA, &[0x40, 0x00]);
    let mut cpu = boot(&mem);

    cpu.step(&mut mem, false); // SWI -> at $4000
    assert_eq!(cpu.r.pc, 0x4000);
    assert_eq!(cpu.shadow_stack.len(), 1);

    let stop = cpu.step_out(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1001);
}

#[test]
fn step_out_breakpoint_short_circuits() {
    // Inside a callee with a BP a few instructions in. step_out must
    // stop at the BP instead of running all the way to the RTS.
    //
    // $1000  8D 02   BSR  +2 -> $1004
    // $1002  12      NOP
    // $1004  12      NOP                (BP here)
    // $1005  39      RTS
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x12, 0x39]);
    let mut cpu = boot(&mem);

    cpu.step(&mut mem, false); // BSR -> at $1004
    let id = cpu.breakpoints.add(0x1004);

    let stop = cpu.step_out(&mut mem, 1000);
    assert_eq!(stop, StepStop::Breakpoint(id));
    assert_eq!(cpu.r.pc, 0x1004);
}

#[test]
fn step_out_runaway_hits_limit() {
    // Callee never returns; step_out gives up after `limit`.
    // $1000  8D 02   BSR  +2 -> $1004
    // $1002  12      NOP
    // $1004  20 FE   BRA  -2    (infinite loop)
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x20, 0xFE]);
    let mut cpu = boot(&mem);

    cpu.step(&mut mem, false); // BSR -> at $1004

    let stop = cpu.step_out(&mut mem, 50);
    assert_eq!(stop, StepStop::Limit);
}

#[test]
fn step_out_pops_through_puls_pc_idiom() {
    // putc-style: callee returns via `puls b,pc`. The em6809 PULS-
    // with-PC fix retires the BSR frame, and step_out uses the
    // shadow frame's return_addr — both pieces have to be working
    // for this to land cleanly on $1002.
    //
    // $1000  8D 02   BSR  +2 -> $1004
    // $1002  12      NOP                (return target)
    // $1003  39      RTS
    // $1004  34 04   PSHS b
    // $1006  35 84   PULS b,pc          (callee return)
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x8D, 0x02, 0x12, 0x39, 0x34, 0x04, 0x35, 0x84]);
    let mut cpu = boot(&mem);

    cpu.step(&mut mem, false); // BSR -> at $1004

    let stop = cpu.step_out(&mut mem, 1000);
    assert_eq!(stop, StepStop::ReturnTarget);
    assert_eq!(cpu.r.pc, 0x1002);
    assert!(cpu.shadow_stack.is_empty());
}
