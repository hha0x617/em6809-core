use std::any::Any;

pub trait Bus: Send {
    fn read8(&mut self, addr: u16) -> u8;
    // Instruction fetch path (for execute permission checks). Default: read8
    fn read8_fetch(&mut self, addr: u16) -> u8 {
        self.read8(addr)
    }
    fn write8(&mut self, addr: u16, data: u8);
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        (false, false, false)
    }
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub struct Memory {
    data: [u8; 0x10000],
}

impl Memory {
    pub fn new() -> Self {
        Self { data: [0; 0x10000] }
    }
    pub fn clear(&mut self, value: u8) {
        self.data.fill(value);
    }
    pub fn load_slice(&mut self, base: u16, bytes: &[u8]) {
        let start = base as usize;
        let end = (base as usize).saturating_add(bytes.len()).min(0x10000);
        let len = end - start;
        self.data[start..end].copy_from_slice(&bytes[..len]);
    }
    pub fn read_slice(&self, start: u16, len: usize) -> &[u8] {
        let s = start as usize;
        let e = (s + len).min(0x10000);
        &self.data[s..e]
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for Memory {
    fn read8(&mut self, addr: u16) -> u8 {
        self.data[addr as usize]
    }
    fn read8_fetch(&mut self, addr: u16) -> u8 {
        self.read8(addr)
    }
    fn write8(&mut self, addr: u16, data: u8) {
        self.data[addr as usize] = data;
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// Wrapper bus that tracks writes within an optional span for event-driven updates
pub struct WriteTrack {
    inner: Box<dyn Bus>,
    dirty_addrs: Vec<u16>,
    span: Option<(u16, u16)>, // track writes only if addr in [start, end)
}

impl WriteTrack {
    pub fn new(inner: Box<dyn Bus>, span: Option<(u16, u16)>) -> Self {
        Self {
            inner,
            dirty_addrs: Vec::new(),
            span,
        }
    }
    pub fn set_span(&mut self, span: Option<(u16, u16)>) {
        self.span = span;
    }
    pub fn take_dirty_addrs(&mut self) -> Vec<u16> {
        let mut out = Vec::new();
        std::mem::swap(&mut out, &mut self.dirty_addrs);
        out.sort_unstable();
        out.dedup();
        out
    }
    pub fn inner_any_mut(&mut self) -> &mut dyn Any {
        self.inner.as_any_mut()
    }
}

impl Bus for WriteTrack {
    fn read8(&mut self, addr: u16) -> u8 {
        self.inner.read8(addr)
    }
    fn read8_fetch(&mut self, addr: u16) -> u8 {
        self.inner.read8_fetch(addr)
    }
    fn write8(&mut self, addr: u16, data: u8) {
        if let Some((s, e)) = self.span {
            if s <= e {
                if addr >= s && addr < e {
                    self.dirty_addrs.push(addr);
                }
            } else {
                // wrapped span (shouldn't happen for program spans, but handle generically)
                if addr >= s || addr < e {
                    self.dirty_addrs.push(addr);
                }
            }
        } else {
            // track all
            self.dirty_addrs.push(addr);
        }
        self.inner.write8(addr, data)
    }
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        self.inner.irq_lines()
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
