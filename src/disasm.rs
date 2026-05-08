#![allow(clippy::uninlined_format_args)]
use crate::bus::Bus;

fn r8<B: Bus + ?Sized>(bus: &mut B, addr: u16) -> u8 {
    bus.read8(addr)
}
fn r16<B: Bus + ?Sized>(bus: &mut B, addr: u16) -> u16 {
    ((r8(bus, addr) as u16) << 8) | (r8(bus, addr.wrapping_add(1)) as u16)
}

fn fmt_hex8(v: u8) -> String {
    format!("${:02X}", v)
}
fn fmt_hex16(v: u16) -> String {
    format!("${:04X}", v)
}

fn reg_name(code: u8) -> &'static str {
    match code & 0x03 {
        0 => "X",
        1 => "Y",
        2 => "U",
        _ => "S",
    }
}

fn reg16_name(code: u8) -> Option<&'static str> {
    match code & 0x0F {
        0x0 => Some("D"),
        0x1 => Some("X"),
        0x2 => Some("Y"),
        0x3 => Some("U"),
        0x4 => Some("S"),
        0x5 => Some("PC"),
        _ => None,
    }
}
fn reg8_name(code: u8) -> Option<&'static str> {
    match code & 0x0F {
        0x8 => Some("A"),
        0x9 => Some("B"),
        0xA => Some("CC"),
        0xB => Some("DP"),
        _ => None,
    }
}

fn decode_indexed<B: Bus + ?Sized>(bus: &mut B, pc: u16) -> (u16, String) {
    let pb = r8(bus, pc);
    let rsel = (pb >> 5) & 0x03;
    if (pb & 0x80) == 0 {
        // 5-bit signed offset ,R
        let off5 = ((pb & 0x1F) as i8) << 3 >> 3;
        if off5 == 0 {
            return (1, format!(",{}", reg_name(rsel)));
        } else {
            let sign = if off5 >= 0 { "+" } else { "-" };
            return (1, format!("{}{},{}", sign, off5.abs(), reg_name(rsel)));
        }
    }
    let form = pb & 0x1F;
    let indirect = (pb & 0x10) != 0;
    let is_pc_rel = rsel == 3 && matches!(form, 0x04 | 0x08 | 0x09 | 0x0B);
    let base = if is_pc_rel { "PC" } else { reg_name(rsel) };
    match form {
        0x04 => {
            // ,R or [ ,R ]
            let t = if indirect {
                format!("[{}]", base)
            } else {
                format!(",{}", base)
            };
            (1, t)
        }
        0x05 => {
            // B,R
            let t = if indirect {
                format!("[B,{}]", base)
            } else {
                format!("B,{}", base)
            };
            (1, t)
        }
        0x06 => {
            // A,R
            let t = if indirect {
                format!("[A,{}]", base)
            } else {
                format!("A,{}", base)
            };
            (1, t)
        }
        0x08 => {
            // n8,R
            let off = r8(bus, pc.wrapping_add(1)) as i8 as i16;
            let t = if off == 0 {
                if indirect {
                    format!("[{},{}]", "", base).replace(" ,", ",")
                } else {
                    format!(",{}", base)
                }
            } else if indirect {
                format!(
                    "[{}{},{}]",
                    if off >= 0 { "+" } else { "-" },
                    off.abs(),
                    base
                )
            } else {
                format!("{}{},{}", if off >= 0 { "+" } else { "-" }, off.abs(), base)
            };
            (2, t)
        }
        0x09 => {
            // n16,R
            let off = r16(bus, pc.wrapping_add(1)) as i16;
            let t = if off == 0 {
                if indirect {
                    format!("[{},{}]", "", base).replace(" ,", ",")
                } else {
                    format!(",{}", base)
                }
            } else if indirect {
                format!(
                    "[{}{},{}]",
                    if off >= 0 { "+" } else { "-" },
                    off.abs(),
                    base
                )
            } else {
                format!("{}{},{}", if off >= 0 { "+" } else { "-" }, off.abs(), base)
            };
            (3, t)
        }
        0x00 => (1, format!(",{}+", base)),
        0x01 => (1, format!(",{}++", base)),
        0x02 => (1, format!(",-{}", base)),
        0x03 => (1, format!(",--{}", base)),
        0x0B => {
            // D,R
            let t = if indirect {
                format!("[D,{}]", base)
            } else {
                format!("D,{}", base)
            };
            (1, t)
        }
        0x0F | 0x1F => {
            // [n16]
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("[{}]", fmt_hex16(addr)))
        }
        _ => (1, format!(",{}(?)", base)),
    }
}

pub fn disasm_one<B: Bus + ?Sized>(bus: &mut B, pc: u16) -> (u16, String) {
    let op = r8(bus, pc);
    match op {
        0x12 => (1, "NOP".to_string()),
        0x39 => (1, "RTS".to_string()),
        0x3B => (1, "RTI".to_string()),
        0x48 => (1, "ASLA".to_string()),
        0x44 => (1, "LSRA".to_string()),
        0x49 => (1, "ROLA".to_string()),
        0x4D => (1, "TSTA".to_string()),
        0x5D => (1, "TSTB".to_string()),
        0x46 => (1, "RORA".to_string()),
        // Accumulator single-byte ops
        0x40 => (1, "NEGA".to_string()),
        0x43 => (1, "COMA".to_string()),
        0x47 => (1, "ASRA".to_string()),
        0x4A => (1, "DECA".to_string()),
        0x4C => (1, "INCA".to_string()),
        0x4F => (1, "CLRA".to_string()),
        0x50 => (1, "NEGB".to_string()),
        0x53 => (1, "COMB".to_string()),
        0x54 => (1, "LSRB".to_string()),
        0x56 => (1, "RORB".to_string()),
        0x57 => (1, "ASRB".to_string()),
        0x58 => (1, "ASLB".to_string()),
        0x59 => (1, "ROLB".to_string()),
        0x5A => (1, "DECB".to_string()),
        0x5C => (1, "INCB".to_string()),
        0x5F => (1, "CLRB".to_string()),
        // Control/arith
        0x13 => (1, "SYNC".to_string()),
        0x19 => (1, "DAA".to_string()),
        0x1A => {
            let m = r8(bus, pc.wrapping_add(1));
            (2, format!("ORCC #{}", fmt_hex8(m)))
        }
        0x1C => {
            let m = r8(bus, pc.wrapping_add(1));
            (2, format!("ANDCC #{}", fmt_hex8(m)))
        }
        0x1D => (1, "SEX".to_string()),
        0x3A => (1, "ABX".to_string()),
        0x3C => {
            let m = r8(bus, pc.wrapping_add(1));
            (2, format!("CWAI #{}", fmt_hex8(m)))
        }
        0x3D => (1, "MUL".to_string()),
        0x3F => (1, "SWI".to_string()),
        0x20 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BRA {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x21 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BRN {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x22 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BHI {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x23 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BLS {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x24 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BCC {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x25 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BCS {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x26 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BNE {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x27 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BEQ {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x28 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BVC {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x29 => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BVS {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x2A => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BPL {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x2B => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BMI {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x2C => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BGE {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x2D => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BLT {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x2E => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BGT {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x2F => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BLE {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x8D => {
            let off = r8(bus, pc.wrapping_add(1)) as i8;
            let tgt = ((pc as i16) + 2 + (off as i16)) as u16;
            (2, format!("BSR {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x16 => {
            let off = r16(bus, pc.wrapping_add(1)) as i16;
            let tgt = ((pc as i32) + 3 + (off as i32)) as u16;
            (3, format!("LBRA {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x17 => {
            let off = r16(bus, pc.wrapping_add(1)) as i16;
            let tgt = ((pc as i32) + 3 + (off as i32)) as u16;
            (3, format!("LBSR {:+} -> {}", off, fmt_hex16(tgt)))
        }
        0x7E => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("JMP {}", fmt_hex16(addr)))
        }
        0x9D => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("JSR <{}", fmt_hex8(zp)))
        }
        0xAD => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("JSR {}", idx))
        }
        0xBD => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("JSR {}", fmt_hex16(addr)))
        }
        // 16-bit D register load/store
        0xCC => {
            let imm = r16(bus, pc.wrapping_add(1));
            (3, format!("LDD #{}", fmt_hex16(imm)))
        }
        0xDC => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("LDD <{}", fmt_hex8(zp)))
        }
        0xEC => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LDD {}", idx))
        }
        0xFC => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("LDD {}", fmt_hex16(addr)))
        }
        0xDD => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("STD <{}", fmt_hex8(zp)))
        }
        0xED => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("STD {}", idx))
        }
        0xFD => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("STD {}", fmt_hex16(addr)))
        }
        // Memory shifts/rotates (indexed)
        0x60 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("NEG {}", idx))
        }
        0x63 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("COM {}", idx))
        }
        0x64 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LSR {}", idx))
        }
        0x66 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ROR {}", idx))
        }
        0x67 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ASR {}", idx))
        }
        0x68 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ASL {}", idx))
        }
        0x69 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ROL {}", idx))
        }
        0x6A => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("DEC {}", idx))
        }
        0x6C => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("INC {}", idx))
        }
        0x6D => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("TST {}", idx))
        }
        0x6E => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("JMP {}", idx))
        }
        0x6F => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("CLR {}", idx))
        }
        // Accumulator A immediate
        0x80 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("SUBA #{}", fmt_hex8(imm)))
        }
        0x81 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("CMPA #{}", fmt_hex8(imm)))
        }
        0x82 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("SBCA #{}", fmt_hex8(imm)))
        }
        0x84 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ANDA #{}", fmt_hex8(imm)))
        }
        0x85 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("BITA #{}", fmt_hex8(imm)))
        }
        0x86 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("LDA #{}", fmt_hex8(imm)))
        }
        0x88 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("EORA #{}", fmt_hex8(imm)))
        }
        0x89 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ADCA #{}", fmt_hex8(imm)))
        }
        0x8A => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ORA #{}", fmt_hex8(imm)))
        }
        0x8B => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ADDA #{}", fmt_hex8(imm)))
        }
        // A direct/indexed/extended
        0x90 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("SUBA <{}", fmt_hex8(zp)))
        }
        0x91 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("CMPA <{}", fmt_hex8(zp)))
        }
        0x92 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("SBCA <{}", fmt_hex8(zp)))
        }
        0x94 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ANDA <{}", fmt_hex8(zp)))
        }
        0x95 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("BITA <{}", fmt_hex8(zp)))
        }
        0x96 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("LDA <{}", fmt_hex8(zp)))
        }
        0x97 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("STA <{}", fmt_hex8(zp)))
        }
        0x98 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("EORA <{}", fmt_hex8(zp)))
        }
        0x99 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ADCA <{}", fmt_hex8(zp)))
        }
        0x9A => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ORA <{}", fmt_hex8(zp)))
        }
        0x9B => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ADDA <{}", fmt_hex8(zp)))
        }
        0xA6 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LDA {}", idx))
        }
        0xA7 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("STA {}", idx))
        }
        0xA0 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("SUBA {}", idx))
        }
        0xA1 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("CMPA {}", idx))
        }
        0xA2 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("SBCA {}", idx))
        }
        0xA4 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ANDA {}", idx))
        }
        0xA5 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("BITA {}", idx))
        }
        0xB0 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("SUBA {}", fmt_hex16(addr)))
        }
        0xB1 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("CMPA {}", fmt_hex16(addr)))
        }
        0xB2 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("SBCA {}", fmt_hex16(addr)))
        }
        0xB4 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ANDA {}", fmt_hex16(addr)))
        }
        0xB5 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("BITA {}", fmt_hex16(addr)))
        }
        0xB6 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("LDA {}", fmt_hex16(addr)))
        }
        0xB7 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("STA {}", fmt_hex16(addr)))
        }
        0xB8 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("EORA {}", fmt_hex16(addr)))
        }
        0xB9 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ADCA {}", fmt_hex16(addr)))
        }
        0xBA => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ORA {}", fmt_hex16(addr)))
        }
        0xBB => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ADDA {}", fmt_hex16(addr)))
        }
        // Accumulator B immediate/direct/indexed/extended
        0xC0 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("SUBB #{}", fmt_hex8(imm)))
        }
        0xC1 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("CMPB #{}", fmt_hex8(imm)))
        }
        0xC2 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("SBCB #{}", fmt_hex8(imm)))
        }
        0xC4 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ANDB #{}", fmt_hex8(imm)))
        }
        0xC5 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("BITB #{}", fmt_hex8(imm)))
        }
        0xC6 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("LDB #{}", fmt_hex8(imm)))
        }
        0xC8 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("EORB #{}", fmt_hex8(imm)))
        }
        0xC9 => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ADCB #{}", fmt_hex8(imm)))
        }
        0xCA => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ORB #{}", fmt_hex8(imm)))
        }
        0xCB => {
            let imm = r8(bus, pc.wrapping_add(1));
            (2, format!("ADDB #{}", fmt_hex8(imm)))
        }
        0xD0 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("SUBB <{}", fmt_hex8(zp)))
        }
        0xD1 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("CMPB <{}", fmt_hex8(zp)))
        }
        0xD2 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("SBCB <{}", fmt_hex8(zp)))
        }
        0xD4 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ANDB <{}", fmt_hex8(zp)))
        }
        0xD5 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("BITB <{}", fmt_hex8(zp)))
        }
        0xD6 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("LDB <{}", fmt_hex8(zp)))
        }
        0xD7 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("STB <{}", fmt_hex8(zp)))
        }
        0xD8 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("EORB <{}", fmt_hex8(zp)))
        }
        0xD9 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ADCB <{}", fmt_hex8(zp)))
        }
        0xDA => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ORB <{}", fmt_hex8(zp)))
        }
        0xDB => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ADDB <{}", fmt_hex8(zp)))
        }
        0xE0 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("SUBB {}", idx))
        }
        0xE1 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("CMPB {}", idx))
        }
        0xE2 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("SBCB {}", idx))
        }
        0xE4 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ANDB {}", idx))
        }
        0xE5 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("BITB {}", idx))
        }
        0xE6 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LDB {}", idx))
        }
        0xE7 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("STB {}", idx))
        }
        0xE8 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("EORB {}", idx))
        }
        0xE9 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ADCB {}", idx))
        }
        0xEA => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ORB {}", idx))
        }
        0xEB => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("ADDB {}", idx))
        }
        0xF0 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("SUBB {}", fmt_hex16(addr)))
        }
        0xF1 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("CMPB {}", fmt_hex16(addr)))
        }
        0xF2 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("SBCB {}", fmt_hex16(addr)))
        }
        0xF4 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ANDB {}", fmt_hex16(addr)))
        }
        0xF5 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("BITB {}", fmt_hex16(addr)))
        }
        0xF6 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("LDB {}", fmt_hex16(addr)))
        }
        0xF7 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("STB {}", fmt_hex16(addr)))
        }
        0xF8 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("EORB {}", fmt_hex16(addr)))
        }
        0xF9 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ADCB {}", fmt_hex16(addr)))
        }
        0xFA => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ORB {}", fmt_hex16(addr)))
        }
        0xFB => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ADDB {}", fmt_hex16(addr)))
        }
        // Index register X load/store variants
        0x9E => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("LDX <{}", fmt_hex8(zp)))
        }
        0xAE => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LDX {}", idx))
        }
        0xBE => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("LDX {}", fmt_hex16(addr)))
        }
        0x9F => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("STX <{}", fmt_hex8(zp)))
        }
        0xAF => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("STX {}", idx))
        }
        0xBF => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("STX {}", fmt_hex16(addr)))
        }
        0x8E => {
            let imm = r16(bus, pc.wrapping_add(1));
            (3, format!("LDX #{}", fmt_hex16(imm)))
        }
        0xCE => {
            let imm = r16(bus, pc.wrapping_add(1));
            (3, format!("LDU #{}", fmt_hex16(imm)))
        }
        0xC3 => {
            let imm = r16(bus, pc.wrapping_add(1));
            (3, format!("ADDD #{}", fmt_hex16(imm)))
        }
        0x83 => {
            let imm = r16(bus, pc.wrapping_add(1));
            (3, format!("SUBD #{}", fmt_hex16(imm)))
        }

        0x34 => {
            let m = r8(bus, pc.wrapping_add(1));
            (2, format!("PSHS #{}", fmt_hex8(m)))
        }
        0x35 => {
            let m = r8(bus, pc.wrapping_add(1));
            (2, format!("PULS #{}", fmt_hex8(m)))
        }
        0x30 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LEAX {}", idx))
        }
        0x31 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LEAY {}", idx))
        }
        0x32 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LEAU {}", idx))
        }
        0x33 => {
            let (add, idx) = decode_indexed(bus, pc.wrapping_add(1));
            (1 + add, format!("LEAS {}", idx))
        }
        // Memory shifts/rotates (direct)
        0x00 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("NEG <{}", fmt_hex8(zp)))
        }
        0x03 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("COM <{}", fmt_hex8(zp)))
        }
        0x04 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("LSR <{}", fmt_hex8(zp)))
        }
        0x06 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ROR <{}", fmt_hex8(zp)))
        }
        0x07 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ASR <{}", fmt_hex8(zp)))
        }
        0x08 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ASL <{}", fmt_hex8(zp)))
        }
        0x09 => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("ROL <{}", fmt_hex8(zp)))
        }
        0x0A => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("DEC <{}", fmt_hex8(zp)))
        }
        0x0C => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("INC <{}", fmt_hex8(zp)))
        }
        0x0D => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("TST <{}", fmt_hex8(zp)))
        }
        0x0E => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("JMP <{}", fmt_hex8(zp)))
        }
        0x0F => {
            let zp = r8(bus, pc.wrapping_add(1));
            (2, format!("CLR <{}", fmt_hex8(zp)))
        }
        // Memory shifts/rotates (extended)
        0x70 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("NEG {}", fmt_hex16(addr)))
        }
        0x73 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("COM {}", fmt_hex16(addr)))
        }
        0x74 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("LSR {}", fmt_hex16(addr)))
        }
        0x76 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ROR {}", fmt_hex16(addr)))
        }
        0x77 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ASR {}", fmt_hex16(addr)))
        }
        0x78 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ASL {}", fmt_hex16(addr)))
        }
        0x79 => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("ROL {}", fmt_hex16(addr)))
        }
        0x7A => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("DEC {}", fmt_hex16(addr)))
        }
        0x7C => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("INC {}", fmt_hex16(addr)))
        }
        0x7D => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("TST {}", fmt_hex16(addr)))
        }
        0x7F => {
            let addr = r16(bus, pc.wrapping_add(1));
            (3, format!("CLR {}", fmt_hex16(addr)))
        }
        0x10 => {
            let op2 = r8(bus, pc.wrapping_add(1));
            match op2 {
                0x3F => (2, "SWI2".to_string()),
                0x8E => {
                    let imm = r16(bus, pc.wrapping_add(2));
                    (4, format!("LDY #{}", fmt_hex16(imm)))
                }
                0x8C => {
                    let imm = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPY #{}", fmt_hex16(imm)))
                }
                0x9C => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("CMPY <{}", fmt_hex8(zp)))
                }
                0x9E => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("LDY <{}", fmt_hex8(zp)))
                }
                0x9F => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("STY <{}", fmt_hex8(zp)))
                }
                0xAE => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("LDY {}", idx))
                }
                0xAC => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("CMPY {}", idx))
                }
                0xAF => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("STY {}", idx))
                }
                0xBE => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("LDY {}", fmt_hex16(addr)))
                }
                0xBC => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPY {}", fmt_hex16(addr)))
                }
                0xBF => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("STY {}", fmt_hex16(addr)))
                }
                // CMPD (page 2)
                0x83 => {
                    let imm = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPD #{}", fmt_hex16(imm)))
                }
                0x93 => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("CMPD <{}", fmt_hex8(zp)))
                }
                0xA3 => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("CMPD {}", idx))
                }
                0xB3 => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPD {}", fmt_hex16(addr)))
                }
                // LDS/STS (page 2)
                0xCE => {
                    let imm = r16(bus, pc.wrapping_add(2));
                    (4, format!("LDS #{}", fmt_hex16(imm)))
                }
                0xDE => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("LDS <{}", fmt_hex8(zp)))
                }
                0xEE => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("LDS {}", idx))
                }
                0xFE => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("LDS {}", fmt_hex16(addr)))
                }
                0xDF => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("STS <{}", fmt_hex8(zp)))
                }
                0xEF => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("STS {}", idx))
                }
                0xFF => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("STS {}", fmt_hex16(addr)))
                }
                // Long conditional branches
                0x21 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBRN {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x22 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBHI {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x23 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBLS {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x24 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBCC {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x25 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBCS {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x26 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBNE {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x27 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBEQ {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x28 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBVC {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x29 => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBVS {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x2A => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBPL {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x2B => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBMI {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x2C => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBGE {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x2D => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBLT {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x2E => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBGT {:+} -> {}", off, fmt_hex16(tgt)))
                }
                0x2F => {
                    let off = r16(bus, pc.wrapping_add(2)) as i16;
                    let tgt = ((pc as i32) + 4 + (off as i32)) as u16;
                    (4, format!("LBLE {:+} -> {}", off, fmt_hex16(tgt)))
                }
                _ => (2, format!("FCB ${:02X}, ${:02X}", op, op2)),
            }
        }
        0x1E => {
            // EXG
            let pb = r8(bus, pc.wrapping_add(1));
            let src = pb >> 4;
            let dst = pb & 0x0F;
            let text = if (src & 0x08) == 0 && (dst & 0x08) == 0 {
                if let (Some(s), Some(d)) = (reg16_name(src), reg16_name(dst)) {
                    format!("EXG {},{}", s, d)
                } else {
                    format!("EXG ${:02X}", pb)
                }
            } else if (src & 0x08) != 0 && (dst & 0x08) != 0 {
                if let (Some(s), Some(d)) = (reg8_name(src), reg8_name(dst)) {
                    format!("EXG {},{}", s, d)
                } else {
                    format!("EXG ${:02X}", pb)
                }
            } else {
                format!("EXG ${:02X}", pb)
            };
            (2, text)
        }
        0x1F => {
            // TFR
            let pb = r8(bus, pc.wrapping_add(1));
            let src = pb >> 4;
            let dst = pb & 0x0F;
            let text = if (src & 0x08) == 0 && (dst & 0x08) == 0 {
                if let Some(s) = reg16_name(src) {
                    if let Some(d) = reg16_name(dst) {
                        format!("TFR {},{}", s, d)
                    } else {
                        format!("TFR ${:02X}", pb)
                    }
                } else {
                    format!("TFR ${:02X}", pb)
                }
            } else if (src & 0x08) != 0 && (dst & 0x08) != 0 {
                if let Some(s) = reg8_name(src) {
                    if let Some(d) = reg8_name(dst) {
                        format!("TFR {},{}", s, d)
                    } else {
                        format!("TFR ${:02X}", pb)
                    }
                } else {
                    format!("TFR ${:02X}", pb)
                }
            } else {
                format!("TFR ${:02X}", pb)
            };
            (2, text)
        }
        0x11 => {
            let op2 = r8(bus, pc.wrapping_add(1));
            match op2 {
                0x3F => (2, "SWI3".to_string()),
                // CMPU/CMPS (page 3)
                0x83 => {
                    let imm = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPU #{}", fmt_hex16(imm)))
                }
                0x93 => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("CMPU <{}", fmt_hex8(zp)))
                }
                0xA3 => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("CMPU {}", idx))
                }
                0xB3 => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPU {}", fmt_hex16(addr)))
                }
                0x8C => {
                    let imm = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPS #{}", fmt_hex16(imm)))
                }
                0x9C => {
                    let zp = r8(bus, pc.wrapping_add(2));
                    (3, format!("CMPS <{}", fmt_hex8(zp)))
                }
                0xAC => {
                    let (add, idx) = decode_indexed(bus, pc.wrapping_add(2));
                    (2 + add, format!("CMPS {}", idx))
                }
                0xBC => {
                    let addr = r16(bus, pc.wrapping_add(2));
                    (4, format!("CMPS {}", fmt_hex16(addr)))
                }
                _ => (2, format!("FCB ${:02X}, ${:02X}", op, op2)),
            }
        }
        _ => (1, format!("FCB ${:02X}", op)),
    }
}

pub fn disasm_one_hex<B: Bus + ?Sized>(bus: &mut B, pc: u16) -> (u16, String) {
    let (len, text) = disasm_one(bus, pc);
    // Gather bytes
    let mut bytes = String::new();
    for i in 0..len {
        let b = r8(bus, pc.wrapping_add(i));
        if !bytes.is_empty() {
            bytes.push(' ');
        }
        bytes.push_str(&format!("{:02X}", b));
    }
    // Ensure a sufficiently wide bytes column so mnemonics align nicely.
    // MC6809 max instruction length is small (<= 5 bytes typically), so pad generously.
    const BYTES_COL_MIN_WIDTH: usize = 20;
    (
        len,
        format!("{: <width$} {}", bytes, text, width = BYTES_COL_MIN_WIDTH),
    )
}

// =============================================================================
// Boundary-anchored window disassembly
// =============================================================================

use crate::debug::InstructionBoundaries;

/// One disassembled line: `(address, mnemonic_text)`.
pub type DisasmLine = (u16, String);

/// Disassemble a window of instructions covering `pc`, anchored on the
/// most recent confirmed instruction boundary at or before `pc`.
///
/// This is the recommended entry point for any UI that displays the
/// disassembly stream around the current PC. Unlike walking forward
/// from `pc - some_constant`, anchoring on `boundaries.floor(pc)`
/// guarantees that the line containing `pc` aligns with the real
/// instruction boundary in the byte stream.
///
/// Behaviour:
/// - The anchor is `boundaries.floor(pc)` if known; otherwise `pc`
///   itself (the PC line is correct, but lines preceding it may
///   not appear because no earlier boundary is known).
/// - The window walks forward from the anchor until it has produced
///   `n_before + 1 + n_after` lines or wraps past 0xFFFF.
/// - If the anchor is far below `pc`, only `n_after` lines after the
///   PC line are kept; surplus lines before the PC line are trimmed
///   to keep at most `n_before`.
///
/// Returns the resulting list of `(addr, text)` lines, in ascending
/// address order. The PC's line is somewhere in the middle (or near
/// the start, when the anchor is close to `pc`).
pub fn disasm_window<B: Bus + ?Sized>(
    bus: &mut B,
    pc: u16,
    boundaries: &InstructionBoundaries,
    n_before: usize,
    n_after: usize,
) -> Vec<DisasmLine> {
    let anchor = boundaries.floor(pc).unwrap_or(pc);

    // To produce up to `n_before` instructions BEFORE `pc`, walk
    // backwards through the known boundary set (NOT byte arithmetic).
    // The earliest of those becomes our walk_start so that subsequent
    // forward walk delivers anchor-aligned lines for the entire
    // pre-PC region.
    let walk_start = if anchor == 0 {
        anchor
    } else {
        let mut start = anchor;
        for b in boundaries
            .iter_range(0, anchor.saturating_sub(1))
            .rev()
            .take(n_before)
        {
            start = b;
        }
        start
    };

    // Walk walk_start → ... forward, stopping when we've covered the
    // PC line plus `n_after` more lines. Cap at a generous upper
    // bound so we can't run away if the bus returns garbage.
    let max_lines = n_before + n_after + 1;
    let mut lines: Vec<DisasmLine> = Vec::with_capacity(max_lines + 8);
    let mut cur = walk_start;
    let mut pc_idx: Option<usize> = None;
    // Hard upper bound on lines we walk: the requested window plus a
    // generous slack for the case where the anchor is many instructions
    // behind `pc`. 6809 instructions are at most 5 bytes, so 0x200
    // bytes of code is comfortably more than `n_before` lines.
    let walk_limit = max_lines + 256;
    while lines.len() < walk_limit {
        let (consumed, text) = disasm_one(bus, cur);
        if cur == pc {
            pc_idx = Some(lines.len());
        }
        lines.push((cur, text));
        if consumed == 0 {
            break;
        }
        let (next, wrapped) = cur.overflowing_add(consumed);
        if wrapped || next <= cur {
            break;
        }
        cur = next;

        // Stop walking once we've placed PC and gathered enough
        // following lines. (We can't stop earlier because we don't
        // know the PC index until we hit it.)
        if let Some(idx) = pc_idx {
            if lines.len() >= idx + 1 + n_after {
                break;
            }
        }
    }

    // Trim surplus pre-PC lines to keep at most n_before.
    if let Some(idx) = pc_idx {
        let start = idx.saturating_sub(n_before);
        let end = (idx + 1 + n_after).min(lines.len());
        lines = lines[start..end].to_vec();
    }
    // If pc_idx is None we never reached `pc` from `anchor`. That
    // means `anchor` was below pc and the anchor's instruction stream
    // skipped past pc. Fall back to a single-line listing at pc.
    if pc_idx.is_none() {
        let (_, text) = disasm_one(bus, pc);
        return vec![(pc, text)];
    }

    lines
}
