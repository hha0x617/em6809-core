use em6809_core::bus::Memory;
use em6809_core::cpu::Cpu;

// Helper to get 16-bit D from A/B (inline at call sites to avoid dead code).

#[test]
fn ldy_imm_and_sty_direct_cycles() {
    let mut mem = Memory::new();
    // LDY #$2000 (0x10 0x8E), STY <$10 (0x10 0x9F 0x10)
    mem.load_slice(0x0000, &[0x10, 0x8E, 0x20, 0x00, 0x10, 0x9F, 0x10]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    let c0 = cpu.cycles;
    cpu.step(&mut mem, false); // LDY #
    assert_eq!(cpu.r.y, 0x2000);
    assert_eq!(
        cpu.cycles - c0,
        4,
        "LDY # should take 4 cycles (prefix + immediate)"
    );
    let c1 = cpu.cycles;
    cpu.step(&mut mem, false); // STY <
    assert_eq!(mem.read_slice(0x0010, 2), &[0x20, 0x00]);
    assert_eq!(
        cpu.cycles - c1,
        5,
        "STY < should take 5 cycles (prefix + direct)"
    );
}

#[test]
fn leax_5bit_and_leay_5bit_set_z() {
    let mut mem = Memory::new();
    // LEAX 5,X ; LEAY -5,Y
    // 5-bit indexed postbyte: bit7=0, bits6..5=rsel, bits4..0=offset(5-bit signed)
    // For X: rsel=0, off=+5 -> pb=0b00_00101 = 0x05
    // For Y: rsel=1, off=-5 (5-bit two's complement=0b11011=0x1B) -> pb=(1<<5)|0x1B = 0x3B
    mem.load_slice(0x0000, &[0x30, 0x05, 0x31, 0x3B]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);
    cpu.r.x = 0x1000;
    cpu.r.y = 0x0005;

    let c0 = cpu.cycles;
    cpu.step(&mut mem, false); // LEAX 5,X
    assert_eq!(cpu.r.x, 0x1005);
    assert_eq!(cpu.cycles - c0, 4, "LEAX 5-bit indexed base cost 4");

    let c1 = cpu.cycles;
    cpu.step(&mut mem, false); // LEAY -5,Y -> 0x0000
    assert_eq!(cpu.r.y, 0x0000);
    // LEA sets Z based on result (Z=1 when result==0)
    assert_ne!(cpu.r.cc & 0x04, 0, "LEAY should set Z when result is zero");
    assert_eq!(cpu.cycles - c1, 4, "LEAY 5-bit indexed base cost 4");
}

#[test]
fn cmpx_and_cmpy_immediate_cycles_and_flags() {
    let mut mem = Memory::new();
    // CMPX #$1234 ; CMPY #$1234
    mem.load_slice(0x0000, &[0x8C, 0x12, 0x34, 0x10, 0x8C, 0x12, 0x34]);
    let mut cpu = Cpu::new();
    cpu.set_pc(0x0000);

    cpu.r.x = 0x1234;
    cpu.r.y = 0x1234;
    let c0 = cpu.cycles;
    cpu.step(&mut mem, false); // CMPX #$1234
    assert_ne!(cpu.r.cc & 0x04, 0, "Z should be set when equal");
    // MC6809 convention: C = 1 on borrow (source > destination), else 0.
    // Equal operands produce no borrow, so C = 0.
    assert_eq!(
        cpu.r.cc & 0x01,
        0x00,
        "C should be clear (no borrow) when equal"
    );
    assert_eq!(cpu.cycles - c0, 4, "CMPX # should be 4 cycles");

    cpu.r.y = 0x1233;
    let c1 = cpu.cycles;
    cpu.step(&mut mem, false); // CMPY #$1234
    assert_eq!(cpu.r.cc & 0x04, 0x00, "Z should clear when not equal");
    // MC6809: Y($1233) - imm($1234) requires a borrow, so C = 1.
    assert_eq!(
        cpu.r.cc & 0x01,
        0x01,
        "C should be set when borrow occurs (Y < imm)"
    );
    assert_eq!(
        cpu.cycles - c1,
        5,
        "CMPY # should be 5 cycles due to prefix"
    );
}
