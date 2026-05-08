// Regression tests for the SWI / SWI2 / SWI3 push-order fix.
//
// Real MC6809: a software interrupt pushes the entire register state
// onto the S stack with PC at the deepest position and CC at the top
// (so RTI's first pull reads CC). NMI / IRQ already followed this
// order; SWI* historically had CC pushed first and PC last, which
// placed CC at the bottom of the saved frame and corrupted the RTI
// round-trip.
//
// These tests pin the spec-conformant layout and the round-trip so a
// future refactor can't silently flip the bytes again.

use em6809_core::bus::Bus;
use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;

const E_FLAG: u8 = 0x80;

fn boot() -> (Memory, Cpu) {
    let mem = Memory::new();
    let mut cpu = Cpu::new();
    cpu.r.pc = 0x1000;
    cpu.r.s = 0x2000;
    (mem, cpu)
}

// ============================================================
// SWI ($3F)
// ============================================================

/// SWI must lay the saved frame out in MC6809 spec order. The S
/// stack grows downward, so the byte pushed last (CC) ends up at
/// the lowest address — that's what RTI pulls first. Layout with
/// S starting at $2000 (full-frame push = 12 bytes, S ends at $1FF4):
///
///   $1FF4 CC        <- top of stack, S after push, RTI's first pull
///   $1FF5 A
///   $1FF6 B
///   $1FF7 DP
///   $1FF8 X hi      $1FF9 X lo
///   $1FFA Y hi      $1FFB Y lo
///   $1FFC U hi      $1FFD U lo
///   $1FFE PC hi     $1FFF PC lo   <- deepest, pushed first
#[test]
fn swi_stack_layout_matches_mc6809_spec() {
    let (mut mem, mut cpu) = boot();
    mem.load_slice(0x1000, &[0x3F]);
    mem.load_slice(0xFFFA, &[0x40, 0x00]);
    mem.load_slice(0x4000, &[0x12]); // NOP — handler body, irrelevant here

    cpu.r.a = 0xAA;
    cpu.r.b = 0xBB;
    cpu.r.dp = 0xCC;
    cpu.r.x = 0x1111;
    cpu.r.y = 0x2222;
    cpu.r.u = 0x3333;
    cpu.r.cc = 0x05; // arbitrary; SWI sets E so the on-stack value is 0x85

    cpu.step(&mut mem, false); // SWI

    assert_eq!(cpu.r.s, 0x1FF4, "S must point to top of saved frame");

    assert_eq!(mem.read8(0x1FF4), 0x85, "[$1FF4] = CC|E (top of stack)");
    assert_eq!(mem.read8(0x1FF5), 0xAA, "[$1FF5] = A");
    assert_eq!(mem.read8(0x1FF6), 0xBB, "[$1FF6] = B");
    assert_eq!(mem.read8(0x1FF7), 0xCC, "[$1FF7] = DP");
    assert_eq!(mem.read8(0x1FF8), 0x11, "[$1FF8] = X hi");
    assert_eq!(mem.read8(0x1FF9), 0x11, "[$1FF9] = X lo");
    assert_eq!(mem.read8(0x1FFA), 0x22, "[$1FFA] = Y hi");
    assert_eq!(mem.read8(0x1FFB), 0x22, "[$1FFB] = Y lo");
    assert_eq!(mem.read8(0x1FFC), 0x33, "[$1FFC] = U hi");
    assert_eq!(mem.read8(0x1FFD), 0x33, "[$1FFD] = U lo");
    assert_eq!(mem.read8(0x1FFE), 0x10, "[$1FFE] = PC hi");
    assert_eq!(mem.read8(0x1FFF), 0x01, "[$1FFF] = PC lo (= $1001 deepest)");
}

#[test]
fn swi_then_rti_restores_pc_and_registers() {
    let (mut mem, mut cpu) = boot();
    mem.load_slice(0x1000, &[0x3F]);
    mem.load_slice(0xFFFA, &[0x40, 0x00]);
    mem.load_slice(0x4000, &[0x3B]); // RTI — return immediately

    cpu.r.a = 0xAA;
    cpu.r.b = 0xBB;
    cpu.r.dp = 0xCC;
    cpu.r.x = 0x1111;
    cpu.r.y = 0x2222;
    cpu.r.u = 0x3333;
    cpu.r.cc = 0x05;

    cpu.step(&mut mem, false); // SWI -> $4000
    cpu.step(&mut mem, false); // RTI -> back to $1001

    assert_eq!(cpu.r.pc, 0x1001, "PC restored to byte after SWI");
    assert_eq!(cpu.r.a, 0xAA);
    assert_eq!(cpu.r.b, 0xBB);
    assert_eq!(cpu.r.dp, 0xCC);
    assert_eq!(cpu.r.x, 0x1111);
    assert_eq!(cpu.r.y, 0x2222);
    assert_eq!(cpu.r.u, 0x3333);
    assert_eq!(cpu.r.cc & !E_FLAG, 0x05 & !E_FLAG, "low CC flags restored");
    assert_eq!(cpu.r.s, 0x2000, "S returned to its pre-SWI value");
}

// ============================================================
// SWI2 ($10 $3F)
// ============================================================

#[test]
fn swi2_then_rti_restores_pc_past_2byte_instruction() {
    let (mut mem, mut cpu) = boot();
    mem.load_slice(0x1000, &[0x10, 0x3F]); // SWI2
    mem.load_slice(0xFFF4, &[0x40, 0x00]); // SWI2 vector
    mem.load_slice(0x4000, &[0x3B]); // RTI

    cpu.r.a = 0x11;
    cpu.r.b = 0x22;

    cpu.step(&mut mem, false); // SWI2
    assert_eq!(cpu.r.pc, 0x4000);
    cpu.step(&mut mem, false); // RTI

    assert_eq!(
        cpu.r.pc, 0x1002,
        "RTI from SWI2 must return past the 2-byte $10 $3F"
    );
    assert_eq!(cpu.r.a, 0x11);
    assert_eq!(cpu.r.b, 0x22);
}

// ============================================================
// SWI3 ($11 $3F)
// ============================================================

#[test]
fn swi3_then_rti_restores_pc_past_2byte_instruction() {
    let (mut mem, mut cpu) = boot();
    mem.load_slice(0x1000, &[0x11, 0x3F]); // SWI3
    mem.load_slice(0xFFF2, &[0x40, 0x00]); // SWI3 vector
    mem.load_slice(0x4000, &[0x3B]); // RTI

    cpu.r.x = 0xDEAD;
    cpu.r.y = 0xBEEF;

    cpu.step(&mut mem, false); // SWI3
    assert_eq!(cpu.r.pc, 0x4000);
    cpu.step(&mut mem, false); // RTI

    assert_eq!(
        cpu.r.pc, 0x1002,
        "RTI from SWI3 must return past the 2-byte $11 $3F"
    );
    assert_eq!(cpu.r.x, 0xDEAD);
    assert_eq!(cpu.r.y, 0xBEEF);
}

// ============================================================
// Cross-check: NMI / IRQ already used the spec-conformant order.
// We pin them too so a future refactor doesn't accidentally regress
// them while reshuffling SWI.
// ============================================================

#[test]
fn nmi_then_rti_restores_pc() {
    let (mut mem, mut cpu) = boot();
    mem.load_slice(0x1000, &[0x12]); // NOP — interrupt is serviced after this
    mem.load_slice(0xFFFC, &[0x40, 0x00]); // NMI vector
    mem.load_slice(0x4000, &[0x3B]); // RTI

    cpu.r.a = 0x77;
    cpu.request_nmi();

    cpu.step(&mut mem, false); // NOP, then NMI services
    assert_eq!(cpu.r.pc, 0x4000);
    cpu.step(&mut mem, false); // RTI

    assert_eq!(cpu.r.pc, 0x1001, "NMI/RTI must round-trip cleanly");
    assert_eq!(cpu.r.a, 0x77);
}

#[test]
fn irq_then_rti_restores_pc() {
    let (mut mem, mut cpu) = boot();
    mem.load_slice(0x1000, &[0x12]);
    mem.load_slice(0xFFF8, &[0x40, 0x00]); // IRQ vector
    mem.load_slice(0x4000, &[0x3B]);

    cpu.r.cc = 0x00; // I bit clear so the IRQ is taken
    cpu.r.b = 0x99;
    cpu.request_irq();

    cpu.step(&mut mem, false); // NOP, then IRQ services
    assert_eq!(cpu.r.pc, 0x4000);
    cpu.step(&mut mem, false); // RTI

    assert_eq!(cpu.r.pc, 0x1001);
    assert_eq!(cpu.r.b, 0x99);
}
