use crate::bus::Bus;
use crate::mmu::Mc6829;

fn parse_hex_or_dec_u16(s: &str) -> Option<u16> {
    if let Some(h) = s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        u16::from_str_radix(h, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}
fn parse_hex_or_dec_usize(s: &str) -> Option<usize> {
    if let Some(h) = s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        usize::from_str_radix(h, 16).ok()
    } else {
        s.parse::<usize>().ok()
    }
}

pub fn apply_mmu_config_from_str(mmu: &mut Mc6829, text: &str) -> Result<(), String> {
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { continue };
        match cmd.to_lowercase().as_str() {
            "task" => {
                if let Some(n) = parts.next() {
                    if let Ok(t) = n.parse::<u8>() {
                        mmu.set_task(t);
                    }
                }
            }
            "mode" => {
                if let Some(m) = parts.next() {
                    match m.to_lowercase().as_str() {
                        "system" | "sys" => mmu.write8(mmu.regs_base + 0x11, 0x01),
                        _ => mmu.write8(mmu.regs_base + 0x11, 0x00),
                    }
                }
            }
            "prot" => {
                if let Some(v) = parts.next() {
                    if let Some(pv) = parse_hex_or_dec_u16(v) {
                        mmu.write8(mmu.regs_base + 0x12, (pv & 0xFF) as u8);
                    }
                }
            }
            "map" => {
                // map P=F
                if let Some(pf) = parts.next() {
                    if let Some(eq) = pf.find('=') {
                        let (p, f) = pf.split_at(eq);
                        let f = &f[1..];
                        if let (Some(pp), Some(ff)) =
                            (parse_hex_or_dec_usize(p), parse_hex_or_dec_u16(f))
                        {
                            mmu.set_map_entry(pp, ff);
                        }
                    }
                }
            }
            "attr" => {
                // attr P=VAL
                if let Some(pv) = parts.next() {
                    if let Some(eq) = pv.find('=') {
                        let (p, v) = pv.split_at(eq);
                        let v = &v[1..];
                        if let (Some(pp), Some(val)) =
                            (parse_hex_or_dec_usize(p), parse_hex_or_dec_u16(v))
                        {
                            let off = 0x18 + (pp as u16);
                            mmu.write8(mmu.regs_base + off, (val & 0xFF) as u8);
                        }
                    }
                }
            }
            _ => {
                return Err(format!("Line {}: unknown directive: {cmd}", lineno + 1));
            }
        }
    }
    Ok(())
}

pub fn apply_preset(mmu: &mut Mc6829, name: &str) -> Result<(), String> {
    // Built-in minimal templates geared for OS-9 Level II experimentation
    match name.to_lowercase().as_str() {
        "os9l2" | "os9l2-basic" => {
            // Task 0, user mode, identity map; protect vectors (top page) from writes
            mmu.set_task(0);
            mmu.identity_map_current();
            // mode user (0), WPROT_EN + IRQ on fault
            mmu.write8(mmu.regs_base + 0x11, 0x00);
            mmu.write8(mmu.regs_base + 0x12, 0x03);
            // attr page 0x0F = WPROT
            mmu.write8(mmu.regs_base + 0x18 + 0x0F, 0x01);
            Ok(())
        }
        "os9l2-sysmap" => {
            // System mode, identity map, vectors protected, NX for low null page
            mmu.set_task(0);
            mmu.identity_map_current();
            // mode system
            mmu.write8(mmu.regs_base + 0x11, 0x01);
            // WPROT_EN | NX_EN | IRQ on fault
            mmu.write8(mmu.regs_base + 0x12, 0x0B);
            // attr: page 0x00 NX, page 0x0F WPROT
            mmu.write8(mmu.regs_base + 0x18, 0x04);
            mmu.write8(mmu.regs_base + 0x18 + 0x0F, 0x01);
            Ok(())
        }
        "os9l2-boot" => {
            // Boot-oriented: system mode, ID map, protect vectors, NX null page, IRQ on fault
            mmu.set_task(0);
            mmu.identity_map_current();
            mmu.write8(mmu.regs_base + 0x11, 0x01); // system mode
            mmu.write8(mmu.regs_base + 0x12, 0x0B); // WPROT_EN | NX_EN | IRQ on fault
            mmu.write8(mmu.regs_base + 0x18, 0x04); // NX on page 0
            mmu.write8(mmu.regs_base + 0x18 + 0x0F, 0x01); // WPROT on vectors page
            Ok(())
        }
        "os9l2-io" => {
            // I/O oriented: system mode, vectors protected, NX for null & IO page, dedicate IO page mapping
            mmu.set_task(0);
            mmu.identity_map_current();
            mmu.write8(mmu.regs_base + 0x11, 0x01); // system mode
                                                    // WPROT_EN | NX_EN | IRQ on fault
            mmu.write8(mmu.regs_base + 0x12, 0x0B);
            // Attributes: NX on page 0x00 (null), WPROT on vectors, NX on 0x0E (I/O)
            mmu.write8(mmu.regs_base + 0x18, 0x04);
            mmu.write8(mmu.regs_base + 0x18 + 0x0F, 0x01);
            mmu.write8(mmu.regs_base + 0x18 + 0x0E, 0x04);
            // Map page 0x0E to a high frame (e.g., 0xFE) to isolate I/O window
            mmu.set_map_entry(0x0E, 0x00FE);
            Ok(())
        }
        "os9l2-ramdisk" => {
            // RAM-disk oriented: user mode, vectors protected, dedicate 16KB for RAM-disk
            mmu.set_task(0);
            mmu.identity_map_current();
            mmu.write8(mmu.regs_base + 0x11, 0x00); // user mode
                                                    // WPROT_EN | IRQ on fault
            mmu.write8(mmu.regs_base + 0x12, 0x03);
            // Protect vectors, leave RAM-disk executable
            mmu.write8(mmu.regs_base + 0x18 + 0x0F, 0x01);
            // Map pages 0x08..0x0B to high frames 0xC0..0xC3
            mmu.set_map_entry(0x08, 0x00C0);
            mmu.set_map_entry(0x09, 0x00C1);
            mmu.set_map_entry(0x0A, 0x00C2);
            mmu.set_map_entry(0x0B, 0x00C3);
            Ok(())
        }
        other => Err(format!("Unknown preset: {other}")),
    }
}

pub fn list_presets() -> &'static [&'static str] {
    &[
        "os9l2",
        "os9l2-basic",
        "os9l2-sysmap",
        "os9l2-boot",
        "os9l2-io",
        "os9l2-ramdisk",
    ]
}
