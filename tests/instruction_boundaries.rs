// Integration tests for the instruction-boundary tracking introduced for
// the disassembly anchor problem (see src/debug.rs).
//
// MC6809 instructions are 1..5 bytes long, so a disassembly view that
// walks `pc - constant` as its starting offset can land on a non-boundary
// byte and produce a desynced listing that hides the current PC and any
// breakpoints. `InstructionBoundaries` lets the view anchor on a
// confirmed boundary instead.

use em6809_core::bus::Memory;
use em6809_core::debug::{linear_sweep, InstructionBoundaries};
use em6809_core::disasm::{disasm_one, disasm_window};

/// Tiny fixture: load three 6809 instructions back-to-back at $1000.
///   $1000  86 42        LDA  #$42       (2 bytes)
///   $1002  C6 7F        LDB  #$7F       (2 bytes)
///   $1004  4F           CLRA            (1 byte)
///   $1005  39           RTS             (1 byte)
fn fixture() -> Memory {
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x86, 0x42, 0xC6, 0x7F, 0x4F, 0x39]);
    mem
}

#[test]
fn linear_sweep_records_each_instruction_start() {
    let mut mem = fixture();
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x1006, &mut b);

    // Expected boundaries: 1000, 1002, 1004, 1005
    assert!(b.contains(0x1000));
    assert!(b.contains(0x1002));
    assert!(b.contains(0x1004));
    assert!(b.contains(0x1005));
    assert_eq!(b.len(), 4);
}

#[test]
fn linear_sweep_skips_addresses_inside_an_instruction() {
    let mut mem = fixture();
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x1006, &mut b);

    // $1001 and $1003 are operand bytes, not instruction starts.
    assert!(!b.contains(0x1001));
    assert!(!b.contains(0x1003));
}

#[test]
fn linear_sweep_no_op_on_empty_range() {
    let mut mem = fixture();
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x1000, &mut b);
    assert!(b.is_empty());
}

#[test]
fn floor_returns_the_largest_boundary_at_or_below() {
    let mut b = InstructionBoundaries::new();
    b.insert(0x1000);
    b.insert(0x1002);
    b.insert(0x1005);

    assert_eq!(b.floor(0x1000), Some(0x1000));
    assert_eq!(b.floor(0x1001), Some(0x1000)); // mid-instruction → previous
    assert_eq!(b.floor(0x1002), Some(0x1002));
    assert_eq!(b.floor(0x1004), Some(0x1002));
    assert_eq!(b.floor(0x1005), Some(0x1005));
    assert_eq!(b.floor(0x10FF), Some(0x1005));
    assert_eq!(b.floor(0x0FFF), None); // nothing at or below
}

#[test]
fn ceil_returns_the_smallest_boundary_at_or_above() {
    let mut b = InstructionBoundaries::new();
    b.insert(0x1000);
    b.insert(0x1005);

    assert_eq!(b.ceil(0x1000), Some(0x1000));
    assert_eq!(b.ceil(0x1001), Some(0x1005));
    assert_eq!(b.ceil(0x1005), Some(0x1005));
    assert_eq!(b.ceil(0x1006), None);
}

#[test]
fn disasm_window_shows_pc_line_when_pc_is_a_known_boundary() {
    let mut mem = fixture();
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x1006, &mut b);

    let lines = disasm_window(&mut mem, 0x1004, &b, 2, 2);
    let pc_line = lines.iter().find(|(a, _)| *a == 0x1004);
    assert!(
        pc_line.is_some(),
        "PC line $1004 should appear in the window: got {:?}",
        lines
    );
    let pc_text = &pc_line.unwrap().1;
    assert!(
        pc_text.contains("CLRA"),
        "expected CLRA mnemonic at $1004, got {:?}",
        pc_text
    );
}

#[test]
fn disasm_window_anchors_correctly_when_pc_is_mid_window() {
    // Anchor at $1000, walk forward, PC at $1004.
    // We ask for 3 lines before and 1 line after; expect:
    //   $1000 LDA #$42
    //   $1002 LDB #$7F
    //   $1004 CLRA   ← PC
    //   $1005 RTS
    let mut mem = fixture();
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x1006, &mut b);

    let lines = disasm_window(&mut mem, 0x1004, &b, 3, 1);
    let addrs: Vec<u16> = lines.iter().map(|(a, _)| *a).collect();
    assert_eq!(addrs, vec![0x1000, 0x1002, 0x1004, 0x1005]);
}

#[test]
fn disasm_window_falls_back_to_pc_when_no_boundary_is_known() {
    // Empty boundaries set: floor() returns None, so the window
    // anchors directly on `pc`. The PC line is still correct.
    let mut mem = fixture();
    let b = InstructionBoundaries::new();

    let lines = disasm_window(&mut mem, 0x1002, &b, 2, 2);
    let pc_line = lines.iter().find(|(a, _)| *a == 0x1002);
    assert!(pc_line.is_some());
    assert!(pc_line.unwrap().1.contains("LDB"));
}

#[test]
fn disasm_window_trims_lines_before_pc_to_n_before_limit() {
    // Even though the anchor walks 4 lines before the PC, only
    // n_before=1 should be retained.
    let mut mem = fixture();
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x1006, &mut b);

    let lines = disasm_window(&mut mem, 0x1005, &b, 1, 0);
    let addrs: Vec<u16> = lines.iter().map(|(a, _)| *a).collect();
    assert_eq!(addrs, vec![0x1004, 0x1005]);
}

#[test]
fn linear_sweep_handles_loaded_data_region_without_panicking() {
    // Code/data mixed: code at $1000, data at $2000. Sweep across
    // both ranges; disasm_one will produce *something* for the data
    // bytes (it always falls through to FCB) but linear_sweep should
    // simply continue without looping or crashing.
    let mut mem = Memory::new();
    mem.load_slice(0x1000, &[0x86, 0x42, 0x39]); // LDA #$42 ; RTS
    mem.load_slice(0x2000, &[0xFF, 0xFF, 0xFF, 0xFF]); // arbitrary bytes
    let mut b = InstructionBoundaries::new();
    linear_sweep(&mut mem, 0x1000, 0x2010, &mut b);

    // Code region boundaries we definitely expect:
    assert!(b.contains(0x1000));
    assert!(b.contains(0x1002)); // RTS

    // Sweep marched into the data region — this is acceptable; the test
    // just asserts we didn't crash and that floor/ceil still work over
    // the recorded set.
    assert!(b.floor(0x1003).is_some());
    assert!(b.ceil(0x2000).is_some());
}

#[test]
fn disasm_one_self_consistency_check() {
    // Sanity: disasm_one on the fixture instructions returns the
    // expected lengths, so the boundary expectations above are
    // grounded in real decoder behaviour rather than guessed.
    let mut mem = fixture();
    assert_eq!(disasm_one(&mut mem, 0x1000).0, 2); // LDA imm
    assert_eq!(disasm_one(&mut mem, 0x1002).0, 2); // LDB imm
    assert_eq!(disasm_one(&mut mem, 0x1004).0, 1); // CLRA
    assert_eq!(disasm_one(&mut mem, 0x1005).0, 1); // RTS
}
