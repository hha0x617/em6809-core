use crate::bus::Bus;
use crate::bus::Memory;

#[derive(Clone, Copy)]
pub enum ImageFormat {
    Binary,
    Srec,
}

pub struct LoadedImage {
    pub loaded_ranges: Vec<(u16, u16)>,
    pub entry: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct ParsedImage {
    pub blocks: Vec<(u16, Vec<u8>)>,
    pub loaded_ranges: Vec<(u16, u16)>,
    pub entry: Option<u16>,
}

pub fn parse_binary(base: u16, data: &[u8]) -> ParsedImage {
    ParsedImage {
        blocks: vec![(base, data.to_vec())],
        loaded_ranges: vec![(base, base.wrapping_add(data.len() as u16))],
        entry: None,
    }
}

pub fn parse_srec(s: &str) -> Result<ParsedImage, String> {
    let mut entry: Option<u16> = None;
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    let mut blocks: Vec<(u16, Vec<u8>)> = Vec::new();
    for (lineno, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('S') {
            return Err(format!("Line {}: Not an S-record", lineno + 1));
        }
        let rtype = line.as_bytes().get(1).copied().unwrap_or(b'?');
        if line.len() < 4 {
            return Err(format!("Line {}: too short", lineno + 1));
        }
        let count = u8::from_str_radix(&line[2..4], 16)
            .map_err(|_| format!("Line {}: bad count", lineno + 1))? as usize;
        match rtype {
            b'0' => { /* header, ignore */ }
            b'1' => {
                if line.len() < 4 + count * 2 {
                    return Err(format!("Line {}: too short for count", lineno + 1));
                }
                if count < 3 {
                    return Err(format!("Line {}: invalid S1 count {}", lineno + 1, count));
                }
                let addr = u16::from_str_radix(&line[4..8], 16)
                    .map_err(|_| format!("Line {}: bad addr", lineno + 1))?;
                let data_bytes = count - 3;
                let mut bytes: Vec<u8> = Vec::with_capacity(data_bytes);
                let mut i = 8;
                for _ in 0..data_bytes {
                    let b = u8::from_str_radix(&line[i..i + 2], 16)
                        .map_err(|_| format!("Line {}: bad data", lineno + 1))?;
                    bytes.push(b);
                    i += 2;
                }
                ranges.push((addr, addr.wrapping_add(bytes.len() as u16)));
                blocks.push((addr, bytes));
            }
            b'2' => {
                if line.len() < 4 + count * 2 {
                    return Err(format!("Line {}: too short for count", lineno + 1));
                }
                if count < 4 {
                    return Err(format!("Line {}: invalid S2 count {}", lineno + 1, count));
                }
                let addr24 = u32::from_str_radix(&line[4..10], 16)
                    .map_err(|_| format!("Line {}: bad addr", lineno + 1))?;
                let addr = (addr24 & 0xFFFF) as u16;
                let data_bytes = count - 4;
                let mut bytes: Vec<u8> = Vec::with_capacity(data_bytes);
                let mut i = 10;
                for _ in 0..data_bytes {
                    let b = u8::from_str_radix(&line[i..i + 2], 16)
                        .map_err(|_| format!("Line {}: bad data", lineno + 1))?;
                    bytes.push(b);
                    i += 2;
                }
                ranges.push((addr, addr.wrapping_add(bytes.len() as u16)));
                blocks.push((addr, bytes));
            }
            b'9' => {
                if line.len() >= 8 {
                    entry = u16::from_str_radix(&line[4..8], 16).ok();
                }
            }
            b'8' => {
                if line.len() >= 10 {
                    let e = u32::from_str_radix(&line[4..10], 16).ok();
                    entry = e.map(|v| (v & 0xFFFF) as u16);
                }
            }
            _ => { /* ignore other types */ }
        }
    }
    Ok(ParsedImage {
        blocks,
        loaded_ranges: ranges,
        entry,
    })
}

pub fn load_binary(mem: &mut Memory, base: u16, data: &[u8]) -> LoadedImage {
    mem.load_slice(base, data);
    LoadedImage {
        loaded_ranges: vec![(base, base.wrapping_add(data.len() as u16))],
        entry: None,
    }
}

pub fn load_binary_bus<B: Bus + ?Sized>(bus: &mut B, base: u16, data: &[u8]) -> LoadedImage {
    for (i, b) in data.iter().enumerate() {
        bus.write8(base.wrapping_add(i as u16), *b);
    }
    LoadedImage {
        loaded_ranges: vec![(base, base.wrapping_add(data.len() as u16))],
        entry: None,
    }
}

pub fn load_srec(mem: &mut Memory, s: &str) -> Result<LoadedImage, String> {
    let mut entry: Option<u16> = None;
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    for (lineno, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('S') {
            return Err(format!("Line {}: Not an S-record", lineno + 1));
        }
        let rtype = line.as_bytes().get(1).copied().unwrap_or(b'?');
        // Parse count (2 hex chars after 'Sx')
        if line.len() < 4 {
            return Err(format!("Line {}: too short", lineno + 1));
        }
        let count = u8::from_str_radix(&line[2..4], 16)
            .map_err(|_| format!("Line {}: bad count", lineno + 1))? as usize;
        match rtype {
            b'0' => { /* header, ignore */ }
            b'1' => {
                // 16-bit address
                // S1 cc aaaa dd.. cksum
                if line.len() < 4 + count * 2 {
                    return Err(format!("Line {}: too short for count", lineno + 1));
                }
                if count < 3 {
                    return Err(format!("Line {}: invalid S1 count {}", lineno + 1, count));
                }
                let addr = u16::from_str_radix(&line[4..8], 16)
                    .map_err(|_| format!("Line {}: bad addr", lineno + 1))?;
                let data_end = 4 + count * 2;
                // count includes addr(2) + data(N) + checksum(1)
                let data_bytes = count - 3; // 2 addr bytes + 1 checksum
                let mut bytes: Vec<u8> = Vec::with_capacity(data_bytes);
                let mut i = 8;
                for _ in 0..data_bytes {
                    let b = u8::from_str_radix(&line[i..i + 2], 16)
                        .map_err(|_| format!("Line {}: bad data", lineno + 1))?;
                    bytes.push(b);
                    i += 2;
                }
                // checksum at i..i+2 (ignored for now)
                mem.load_slice(addr, &bytes);
                ranges.push((addr, addr.wrapping_add(bytes.len() as u16)));
                if data_end != i + 2 {
                    let _ = data_end;
                }
            }
            b'2' => {
                // 24-bit address (we only keep low 16)
                // S2 cc aaaaaa dd.. cksum
                if line.len() < 4 + count * 2 {
                    return Err(format!("Line {}: too short for count", lineno + 1));
                }
                if count < 4 {
                    return Err(format!("Line {}: invalid S2 count {}", lineno + 1, count));
                }
                let addr24 = u32::from_str_radix(&line[4..10], 16)
                    .map_err(|_| format!("Line {}: bad addr", lineno + 1))?;
                let addr = (addr24 & 0xFFFF) as u16;
                let data_bytes = count - 4; // 3 addr + 1 checksum
                let mut bytes: Vec<u8> = Vec::with_capacity(data_bytes);
                let mut i = 10;
                for _ in 0..data_bytes {
                    let b = u8::from_str_radix(&line[i..i + 2], 16)
                        .map_err(|_| format!("Line {}: bad data", lineno + 1))?;
                    bytes.push(b);
                    i += 2;
                }
                mem.load_slice(addr, &bytes);
                ranges.push((addr, addr.wrapping_add(bytes.len() as u16)));
            }
            b'9' => {
                // termination with 16-bit entry
                if line.len() >= 8 {
                    entry = u16::from_str_radix(&line[4..8], 16).ok();
                }
            }
            b'8' => {
                // termination with 24-bit entry
                if line.len() >= 10 {
                    let e = u32::from_str_radix(&line[4..10], 16).ok();
                    entry = e.map(|v| (v & 0xFFFF) as u16);
                }
            }
            _ => { /* ignore other types */ }
        }
    }
    Ok(LoadedImage {
        loaded_ranges: ranges,
        entry,
    })
}

pub fn load_srec_bus<B: Bus + ?Sized>(bus: &mut B, s: &str) -> Result<LoadedImage, String> {
    let mut entry: Option<u16> = None;
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    for (lineno, line) in s.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('S') {
            return Err(format!("Line {}: Not an S-record", lineno + 1));
        }
        let rtype = line.as_bytes().get(1).copied().unwrap_or(b'?');
        if line.len() < 4 {
            return Err(format!("Line {}: too short", lineno + 1));
        }
        let count = u8::from_str_radix(&line[2..4], 16)
            .map_err(|_| format!("Line {}: bad count", lineno + 1))? as usize;
        match rtype {
            b'1' => {
                if line.len() < 4 + count * 2 {
                    return Err(format!("Line {}: too short for count", lineno + 1));
                }
                if count < 3 {
                    return Err(format!("Line {}: invalid S1 count {}", lineno + 1, count));
                }
                let addr = u16::from_str_radix(&line[4..8], 16)
                    .map_err(|_| format!("Line {}: bad addr", lineno + 1))?;
                let data_bytes = count - 3;
                let mut bytes: Vec<u8> = Vec::with_capacity(data_bytes);
                let mut i = 8;
                for _ in 0..data_bytes {
                    let b = u8::from_str_radix(&line[i..i + 2], 16)
                        .map_err(|_| format!("Line {}: bad data", lineno + 1))?;
                    bytes.push(b);
                    i += 2;
                }
                for (ofs, b) in bytes.iter().enumerate() {
                    bus.write8(addr.wrapping_add(ofs as u16), *b);
                }
                ranges.push((addr, addr.wrapping_add(bytes.len() as u16)));
            }
            b'2' => {
                if line.len() < 4 + count * 2 {
                    return Err(format!("Line {}: too short for count", lineno + 1));
                }
                if count < 4 {
                    return Err(format!("Line {}: invalid S2 count {}", lineno + 1, count));
                }
                let addr24 = u32::from_str_radix(&line[4..10], 16)
                    .map_err(|_| format!("Line {}: bad addr", lineno + 1))?;
                let addr = (addr24 & 0xFFFF) as u16;
                let data_bytes = count - 4;
                let mut bytes: Vec<u8> = Vec::with_capacity(data_bytes);
                let mut i = 10;
                for _ in 0..data_bytes {
                    let b = u8::from_str_radix(&line[i..i + 2], 16)
                        .map_err(|_| format!("Line {}: bad data", lineno + 1))?;
                    bytes.push(b);
                    i += 2;
                }
                for (ofs, b) in bytes.iter().enumerate() {
                    bus.write8(addr.wrapping_add(ofs as u16), *b);
                }
                ranges.push((addr, addr.wrapping_add(bytes.len() as u16)));
            }
            b'9' => {
                if line.len() >= 8 {
                    entry = u16::from_str_radix(&line[4..8], 16).ok();
                }
            }
            b'8' => {
                if line.len() >= 10 {
                    let e = u32::from_str_radix(&line[4..10], 16).ok();
                    entry = e.map(|v| (v & 0xFFFF) as u16);
                }
            }
            _ => {}
        }
    }
    Ok(LoadedImage {
        loaded_ranges: ranges,
        entry,
    })
}
