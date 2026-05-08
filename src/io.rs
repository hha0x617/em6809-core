use crate::bus::Bus;
use once_cell::sync::OnceCell;
use std::any::Any;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub trait Device: Send {
    fn contains(&self, addr: u16) -> bool;
    fn read8(&mut self, addr: u16) -> u8;
    fn write8(&mut self, addr: u16, data: u8);
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        (false, false, false)
    }
}

// ------------------------------------------------------------
// Console (UART-like) device
//
// Implements the Motorola MC6850 ACIA register layout:
//   +0: read = Status Register, write = Control Register
//   +1: read = Receive Data Register, write = Transmit Data Register
//
// Required by `Hha Forth`, `Hha Lisp`, the NetBSD MVME147 boot ROM,
// etc.  An older `Simple` layout (data@+0, status@+1) used to live
// alongside this one but was removed — it had no real-hardware
// counterpart and only one or two demo programs depended on it.
// ------------------------------------------------------------

static CONSOLE_LOG: AtomicBool = AtomicBool::new(false);
pub fn set_console_log(on: bool) {
    CONSOLE_LOG.store(on, Ordering::SeqCst);
}

// GUI console output routing (VT100 window)
static CONSOLE_GUI_ENABLED: AtomicBool = AtomicBool::new(false);
static CONSOLE_GUI_ACTIVE: AtomicBool = AtomicBool::new(false);
static CONSOLE_STDOUT_ENABLED: AtomicBool = AtomicBool::new(true);
static CONSOLE_GUI_BUF: OnceCell<Mutex<Vec<u8>>> = OnceCell::new();
static CONSOLE_REPAINT_CB: OnceCell<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = OnceCell::new();

pub fn set_console_gui_enabled(on: bool) {
    CONSOLE_GUI_ENABLED.store(on, Ordering::SeqCst);
}

pub fn set_console_gui_active(on: bool) {
    CONSOLE_GUI_ACTIVE.store(on, Ordering::SeqCst);
}

pub fn set_console_stdout_enabled(on: bool) {
    CONSOLE_STDOUT_ENABLED.store(on, Ordering::SeqCst);
}

pub fn take_console_gui_bytes() -> Vec<u8> {
    if let Some(m) = CONSOLE_GUI_BUF.get() {
        if let Ok(mut g) = m.lock() {
            let out = g.clone();
            g.clear();
            return out;
        }
    }
    Vec::new()
}

pub fn console_gui_pending_len() -> usize {
    if let Some(m) = CONSOLE_GUI_BUF.get() {
        if let Ok(g) = m.lock() {
            return g.len();
        }
    }
    0
}

pub fn set_console_repaint_callback(cb: Option<Arc<dyn Fn() + Send + Sync>>) {
    let slot = CONSOLE_REPAINT_CB.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = slot.lock() {
        *g = cb;
    }
}

static BLOCK_LOG: AtomicBool = AtomicBool::new(false);
pub fn set_block_log(on: bool) {
    BLOCK_LOG.store(on, Ordering::SeqCst);
}

pub struct Mc6850Dev {
    pub base: u16,

    // Control register decoded fields.
    cds: u8,
    tx_irq_enable: bool,
    rie: bool,

    // Receive side.
    rx: VecDeque<u8>,
    rx_wm: usize,
    rdrf: bool,
    rx_overrun: bool,

    // Transmit side.  Instant-transmit emulation keeps TDRE=1 after a
    // write because the host side sees the byte go out immediately.
    tdre: bool,

    // Master-reset latch: TDRE stays 0 until the first non-reset CR write.
    in_master_reset: bool,

    // IRQ pacing (matches ConsoleDev so the GUI timing feels identical).
    irq_hold_cycles: u32,
    irq_countdown: u32,

    // Output routing: byte-for-byte duplicate of ConsoleDev's sink so both
    // device types work with the same `set_out_file` / `set_tee_stderr` /
    // `set_flush_*` CLI and VT100-window wiring.
    out_file: Option<File>,
    tee_stderr: bool,
    flush_every: Option<usize>,
    flush_on_newline: bool,
    flush_count: usize,
    local_echo: bool,
    utf8_buf: Vec<u8>,
    utf8_need: usize,
}

impl Mc6850Dev {
    pub fn new(base: u16) -> Self {
        Self {
            base,
            cds: 0b11,
            tx_irq_enable: false,
            rie: false,
            rx: VecDeque::new(),
            rx_wm: 1,
            rdrf: false,
            rx_overrun: false,
            // Start in a "post-master-reset" ready state so hot-swap from
            // ConsoleDev works even when the guest is already past its init
            // sequence.  A guest that later issues a real Master Reset
            // (CDS=11) will go through in_master_reset briefly and return
            // to ready on the next control write — identical to bootstrap.
            tdre: true,
            in_master_reset: false,
            irq_hold_cycles: 0,
            irq_countdown: 0,
            out_file: None,
            tee_stderr: false,
            flush_every: Some(1),
            flush_on_newline: false,
            flush_count: 0,
            local_echo: false,
            utf8_buf: Vec::with_capacity(8),
            utf8_need: 0,
        }
    }

    // --- Public setters mirroring ConsoleDev so CLI wiring is symmetric. -----
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.receive(b);
            if CONSOLE_LOG.load(Ordering::SeqCst) {
                eprintln!("[console-rx] queued ${:02X}", b);
            }
        }
    }
    pub fn set_rx_watermark(&mut self, wm: usize) {
        self.rx_wm = wm.max(1);
    }
    pub fn set_irq_hold_cycles(&mut self, n: u32) {
        self.irq_hold_cycles = n;
    }
    pub fn set_out_file(&mut self, path: &str) {
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(path) {
            self.out_file = Some(f);
        }
    }
    pub fn set_tee_stderr(&mut self, on: bool) {
        self.tee_stderr = on;
    }
    pub fn set_flush_every(&mut self, n: usize) {
        self.flush_every = if n == 0 { None } else { Some(n) };
    }
    pub fn set_flush_on_newline(&mut self, on: bool) {
        self.flush_on_newline = on;
    }
    pub fn set_local_echo(&mut self, on: bool) {
        self.local_echo = on;
    }

    /// Boot-script compatibility shim: write `v` to the Control
    /// Register the same way the guest would by writing to `base+0`.
    /// Useful for `Action::ConCtrl` which existed when both Simple and
    /// MC6850 console devices were supported and is still accepted in
    /// boot scripts targeting MC6850 (the only flavor that survives).
    pub fn set_ctrl(&mut self, v: u8) {
        let base = self.base;
        Device::write8(self, base, v);
    }

    /// Boot-script compatibility shim — accepted for backward
    /// compatibility but a no-op: the real MC6850 ACIA only drives the
    /// IRQ pin, not FIRQ.  A diagnostic is emitted when the script
    /// asks for FIRQ-on so the user notices.
    pub fn set_firq(&mut self, on: bool) {
        if on {
            eprintln!("[console] FIRQ requested but MC6850 only routes to IRQ; ignoring");
        }
    }

    /// Host deposits an RX byte (ACCEPT reads land here).
    fn receive(&mut self, ch: u8) {
        if self.in_master_reset {
            return;
        }
        const RX_FIFO_MAX: usize = 16;
        if self.rx.len() >= RX_FIFO_MAX {
            self.rx_overrun = true;
            return;
        }
        self.rx.push_back(ch);
        self.rdrf = true;
    }

    fn irq_level(&self) -> bool {
        (self.rdrf && self.rie) || (self.tdre && self.tx_irq_enable)
    }

    // --- Output sink (byte-for-byte duplicate of ConsoleDev::push_stdout_byte). --
    fn push_stdout_byte(&mut self, b: u8) {
        if let Some(f) = self.out_file.as_mut() {
            let _ = f.write_all(&[b]);
        }
        if CONSOLE_GUI_ACTIVE.load(Ordering::SeqCst) && CONSOLE_GUI_ENABLED.load(Ordering::SeqCst) {
            if let Some(m) = CONSOLE_GUI_BUF.get() {
                if let Ok(mut g) = m.lock() {
                    g.push(b);
                }
            } else {
                let _ = CONSOLE_GUI_BUF.set(Mutex::new(vec![b]));
            }
            if let Some(cb) = CONSOLE_REPAINT_CB.get() {
                if let Ok(cb) = cb.lock() {
                    if let Some(cb) = cb.as_ref() {
                        cb();
                    }
                }
            }
        }
        let stdout_enabled = CONSOLE_STDOUT_ENABLED.load(Ordering::SeqCst)
            || !CONSOLE_GUI_ACTIVE.load(Ordering::SeqCst);
        if stdout_enabled && b < 0x80 && self.utf8_need == 0 {
            print!("{}", b as char);
        } else if stdout_enabled {
            if self.utf8_need == 0 {
                self.utf8_need = match b {
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF7 => 4,
                    _ => 1,
                };
            }
            self.utf8_buf.push(b);
            if self.utf8_buf.len() >= self.utf8_need.max(1) {
                let s = String::from_utf8_lossy(&self.utf8_buf);
                print!("{s}");
                self.utf8_buf.clear();
                self.utf8_need = 0;
            }
        }
        let mut need_flush = self.flush_on_newline && b == b'\n';
        if let Some(n) = self.flush_every {
            self.flush_count = self.flush_count.wrapping_add(1);
            if self.flush_count.is_multiple_of(n) {
                need_flush = true;
            }
        }
        if need_flush {
            if stdout_enabled {
                let _ = std::io::stdout().flush();
            }
            if let Some(f) = self.out_file.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

impl Device for Mc6850Dev {
    fn contains(&self, addr: u16) -> bool {
        addr == self.base || addr == self.base.wrapping_add(1)
    }
    fn read8(&mut self, addr: u16) -> u8 {
        if addr == self.base {
            // Status Register
            let mut s = 0u8;
            if self.rdrf {
                s |= 0x01;
            }
            if self.tdre {
                s |= 0x02;
            }
            if self.rx_overrun {
                s |= 0x20;
            }
            if self.irq_level() {
                s |= 0x80;
            }
            s
        } else {
            // Receive Data Register
            let v = self.rx.pop_front().unwrap_or(0);
            self.rdrf = !self.rx.is_empty();
            self.rx_overrun = false;
            if self.local_echo {
                self.push_stdout_byte(v);
            }
            v
        }
    }
    fn write8(&mut self, addr: u16, data: u8) {
        if addr == self.base {
            // Control Register
            let cds = data & 0b11;
            let tc = (data >> 5) & 0b11;
            let rie = (data & 0x80) != 0;
            self.cds = cds;
            self.tx_irq_enable = tc == 0b01;
            self.rie = rie;
            if cds == 0b11 {
                // Master reset
                self.in_master_reset = true;
                self.tdre = false;
                self.rdrf = false;
                self.rx_overrun = false;
                self.rx.clear();
                return;
            }
            if self.in_master_reset {
                self.in_master_reset = false;
                self.tdre = true;
            }
        } else {
            // Transmit Data Register — instant transmit.
            if self.in_master_reset {
                return;
            }
            if CONSOLE_LOG.load(Ordering::SeqCst) {
                let _ = writeln!(
                    std::io::stderr(),
                    "[console-tx] addr={addr:04X} data={data:02X}"
                );
            }
            if self.tee_stderr {
                let _ = std::io::stderr().write_all(&[data]);
            }
            self.push_stdout_byte(data);
            self.tdre = true;
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        let level = self.irq_level();
        let mut on = level;
        if on {
            self.irq_countdown = self.irq_hold_cycles;
        } else if self.irq_countdown > 0 {
            self.irq_countdown -= 1;
            on = true;
        }
        // IRQ routing (always IRQ pin for MC6850; FIRQ not part of the chip).
        (false, false, on)
    }
}

// ------------------------------------------------------------
// Simple Block (disk) device with 512-byte sectors
// I/O map (base .. base+7):
//   +0: DATA      (R/W) data port for a sector buffer
//   +1: STATUS    (R)  bit0=BUSY, bit1=DRDY, bit2=ERR; write ignored
//   +2: COMMAND   (W)  0x00=NOP, 0x01=READ, 0x02=WRITE, 0xFF=RESET; (R) last cmd
//   +3: SECCNT    (R/W) sector count; currently only 1 is supported
//   +4..+7: LBA0..LBA3 (R/W) 32-bit LBA (little-endian: LBA0=LSB)
// Behavior:
//   - READ: loads 512 bytes at LBA into internal buffer; sets DRDY; ERR if OOB (fills zeros)
//   - WRITE: writes current buffer to LBA; grows image as needed; clears DRDY; sets ERR on failure
//   - DATA port increments an internal index on each byte read/write; READ clears DRDY after 512 reads.
//   - SECCNT is accepted but only the value 1 is honored.
// Backing store:
//   - In-memory Vec<u8>, optionally mirrored to a file path on writes.
// Limitations:
//   - No IRQ/FIRQ; no multi-sector I/O; no timing/busy delays.
#[derive(Debug)]
pub struct BlockDev {
    pub base: u16,
    sector_size: usize,
    lba: u32,
    sec_cnt: u8,
    status: u8,
    last_cmd: u8,
    buf: Vec<u8>,
    buf_idx: usize,
    image: Vec<u8>,
    backing_path: Option<String>,
    // Transfer state
    op_read: bool,
    op_write: bool,
    xfer_remaining: usize,
    // IRQ config/state
    irq_enable: bool,
    use_firq: bool,
    irq_hold_cycles: u32,
    irq_countdown: u32,
    last_data_byte: u8,
    state_dirty: bool,
}

impl BlockDev {
    pub fn new(base: u16) -> Self {
        let sector_size = 512usize;
        Self {
            base,
            sector_size,
            lba: 0,
            sec_cnt: 1,
            status: 0,
            last_cmd: 0,
            buf: vec![0u8; sector_size],
            buf_idx: 0,
            image: Vec::new(),
            backing_path: None,
            op_read: false,
            op_write: false,
            xfer_remaining: 0,
            irq_enable: false,
            use_firq: false,
            irq_hold_cycles: 0,
            irq_countdown: 0,
            last_data_byte: 0,
            state_dirty: true,
        }
    }
    pub fn set_backing_file(&mut self, path: &str) {
        match std::fs::read(path) {
            Ok(data) => {
                self.image = data;
                self.backing_path = Some(path.to_string());
            }
            Err(_) => {
                // If file read fails, keep empty image but remember the path for future writes.
                self.backing_path = Some(path.to_string());
            }
        }
    }
    pub fn backing_file(&self) -> Option<&str> {
        self.backing_path.as_deref()
    }
    pub fn set_image(&mut self, data: Vec<u8>) {
        self.image = data;
    }
    pub fn last_cmd(&self) -> u8 {
        self.last_cmd
    }
    pub fn last_data(&self) -> u8 {
        self.last_data_byte
    }
    pub fn status(&self) -> u8 {
        self.status
    }
    pub fn take_dirty(&mut self) -> bool {
        if self.state_dirty {
            self.state_dirty = false;
            true
        } else {
            false
        }
    }
    fn mark_dirty(&mut self) {
        self.state_dirty = true;
    }
    pub fn set_irq_hold_cycles(&mut self, n: u32) {
        self.irq_hold_cycles = n;
    }
    pub fn set_irq_enable(&mut self, on: bool) {
        self.irq_enable = on;
    }
    pub fn set_firq(&mut self, on: bool) {
        self.use_firq = on;
    }
    fn contains_reg(&self, addr: u16) -> bool {
        addr.wrapping_sub(self.base) < 8
    }
    fn set_status_busy(&mut self, on: bool) {
        let prev = self.status;
        if on {
            self.status |= 0x01;
        } else {
            self.status &= !0x01;
        }
        if self.status != prev {
            self.mark_dirty();
        }
    }
    fn set_status_drdy(&mut self, on: bool) {
        let prev = self.status;
        if on {
            self.status |= 0x02;
        } else {
            self.status &= !0x02;
        }
        if self.status != prev {
            self.mark_dirty();
        }
    }
    fn set_status_err(&mut self, on: bool) {
        let prev = self.status;
        if on {
            self.status |= 0x04;
        } else {
            self.status &= !0x04;
        }
        if self.status != prev {
            self.mark_dirty();
        }
    }
    #[allow(dead_code)]
    fn image_len_sectors(&self) -> u64 {
        (self.image.len() as u64) / (self.sector_size as u64)
    }
    fn read_sector_into_buf(&mut self) {
        self.set_status_err(false);
        self.set_status_busy(true);
        let ss = self.sector_size as u64;
        let lba = self.lba as u64;
        let start = lba.saturating_mul(ss) as usize;
        let end = start.saturating_add(self.sector_size);
        if end <= self.image.len() {
            self.buf.copy_from_slice(&self.image[start..end]);
        } else {
            // Out of range: zero-fill and mark error
            for b in self.buf.iter_mut() {
                *b = 0;
            }
            self.set_status_err(true);
        }
        self.buf_idx = 0;
        self.set_status_busy(false);
        self.set_status_drdy(true);
    }
    fn write_buf_to_sector(&mut self) {
        self.set_status_err(false);
        self.set_status_busy(true);
        let ss = self.sector_size as u64;
        let lba = self.lba as u64;
        let start = lba.saturating_mul(ss) as usize;
        let end = start.saturating_add(self.sector_size);
        if end > self.image.len() {
            self.image.resize(end, 0);
        }
        let mut io_error = false;
        if start < self.image.len() && end <= self.image.len() {
            self.image[start..end].copy_from_slice(&self.buf);
            if let Some(path) = &self.backing_path {
                // Attempt to mirror the written sector to the backing file
                if let Ok(mut f) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .truncate(false)
                    .create(true)
                    .open(path)
                {
                    if let Ok(meta) = f.metadata() {
                        if meta.len() < end as u64 {
                            // Extend file to required size
                            if f.set_len(end as u64).is_err() {
                                io_error = true;
                            }
                        }
                    }
                    if f.seek(SeekFrom::Start(start as u64)).is_err()
                        || f.write_all(&self.buf).is_err()
                    {
                        io_error = true;
                    }
                } else {
                    io_error = true;
                }
            }
        } else {
            io_error = true;
        }
        self.set_status_busy(false);
        self.buf_idx = 0;
        if io_error {
            self.set_status_err(true);
        }
    }
}

impl Device for BlockDev {
    fn contains(&self, addr: u16) -> bool {
        self.contains_reg(addr)
    }
    fn read8(&mut self, addr: u16) -> u8 {
        let off = addr.wrapping_sub(self.base);
        match off {
            0 => {
                // DATA
                if (self.status & 0x02) != 0 && self.buf_idx < self.sector_size {
                    let b = self.buf[self.buf_idx];
                    self.buf_idx += 1;
                    self.last_data_byte = b;
                    self.mark_dirty();
                    self.xfer_remaining = self.xfer_remaining.saturating_sub(1);
                    if self.buf_idx >= self.sector_size {
                        // Sector exhausted; if more remains and READ op, load next sector
                        if self.op_read && self.xfer_remaining > 0 {
                            self.lba = self.lba.wrapping_add(1);
                            self.read_sector_into_buf();
                        } else {
                            self.set_status_drdy(false);
                            self.op_read = false;
                            // Completed read transfer -> pulse IRQ hold
                            if self.irq_enable {
                                self.irq_countdown = self.irq_hold_cycles;
                            }
                        }
                    }
                    b
                } else {
                    self.last_data_byte = 0;
                    self.mark_dirty();
                    0x00
                }
            }
            1 => {
                let s = self.status;
                self.set_status_err(false);
                s
            } // STATUS read clears ERR
            2 => self.last_cmd, // COMMAND (readback)
            3 => self.sec_cnt,  // SECCNT (readback)
            4 => (self.lba & 0x000000FF) as u8,
            5 => ((self.lba >> 8) & 0xFF) as u8,
            6 => ((self.lba >> 16) & 0xFF) as u8,
            7 => ((self.lba >> 24) & 0xFF) as u8,
            _ => 0xFF,
        }
    }
    fn write8(&mut self, addr: u16, data: u8) {
        let off = addr.wrapping_sub(self.base);
        match off {
            0 => {
                // DATA write: stage into buffer
                if self.buf_idx < self.sector_size {
                    self.buf[self.buf_idx] = data;
                    self.buf_idx += 1;
                    self.last_data_byte = data;
                    self.mark_dirty();
                    if self.op_write {
                        // Streaming multi-sector write
                        if self.buf_idx >= self.sector_size {
                            self.write_buf_to_sector();
                            self.lba = self.lba.wrapping_add(1);
                            self.xfer_remaining =
                                self.xfer_remaining.saturating_sub(self.sector_size);
                            // Prepare for next sector if more remains
                            if self.xfer_remaining > 0 {
                                // Buffer will be refilled by further DATA writes
                                self.buf.fill(0);
                                self.buf_idx = 0;
                            } else {
                                self.op_write = false;
                                self.set_status_busy(false);
                                if self.irq_enable {
                                    self.irq_countdown = self.irq_hold_cycles;
                                }
                            }
                        }
                    }
                }
            }
            1 => { /* STATUS write ignored */ }
            2 => {
                // COMMAND
                if self.last_cmd != data {
                    self.last_cmd = data;
                    self.mark_dirty();
                } else {
                    self.last_cmd = data;
                }
                if BLOCK_LOG.load(Ordering::SeqCst) {
                    eprintln!(
                        "[block] CMD=${:02X} LBA=${:08X} SECCNT=${:02X} irq={}",
                        data, self.lba, self.sec_cnt, self.irq_enable
                    );
                }
                match data {
                    0x00 => { /* NOP */ }
                    0x01 => {
                        // READ one sector
                        let cnt = core::cmp::max(1u8, self.sec_cnt) as usize;
                        self.xfer_remaining = cnt.saturating_mul(self.sector_size);
                        self.op_read = true;
                        self.op_write = false;
                        self.read_sector_into_buf();
                    }
                    0x02 => {
                        // WRITE one sector (commit current buffer)
                        let cnt = core::cmp::max(1u8, self.sec_cnt) as usize;
                        self.xfer_remaining = cnt.saturating_mul(self.sector_size);
                        self.op_write = true;
                        self.op_read = false;
                        self.set_status_busy(true);
                        // If buffer already has one sector worth (rare), start committing immediately
                        if self.buf_idx >= self.sector_size {
                            self.write_buf_to_sector();
                            self.lba = self.lba.wrapping_add(1);
                            self.xfer_remaining =
                                self.xfer_remaining.saturating_sub(self.sector_size);
                            self.buf.fill(0);
                            self.buf_idx = 0;
                        }
                    }
                    0xFF => {
                        // RESET
                        self.status = 0;
                        self.buf_idx = 0;
                        self.last_cmd = 0;
                        self.op_read = false;
                        self.op_write = false;
                        self.xfer_remaining = 0;
                        self.irq_countdown = 0;
                    }
                    _ => {
                        // Unknown command -> set error
                        self.set_status_err(true);
                    }
                }
            }
            3 => {
                self.sec_cnt = if data == 0 { 1 } else { data };
            }
            4 => {
                self.lba = (self.lba & 0xFFFFFF00) | (data as u32);
            }
            5 => {
                self.lba = (self.lba & 0xFFFF00FF) | ((data as u32) << 8);
            }
            6 => {
                self.lba = (self.lba & 0xFF00FFFF) | ((data as u32) << 16);
            }
            7 => {
                self.lba = (self.lba & 0x00FFFFFF) | ((data as u32) << 24);
            }
            8 => {
                // CONTROL: bit0 IRQ_EN, bit1 FIRQ, bit2 clear ERR
                self.irq_enable = (data & 0x01) != 0;
                self.use_firq = (data & 0x02) != 0;
                if (data & 0x04) != 0 {
                    self.set_status_err(false);
                }
            }
            _ => {}
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        // IRQ when DRDY (read data available) or ERR set, or when a write completes (signaled by op_write false and last_cmd==0x02 just finished).
        let drdy = (self.status & 0x02) != 0;
        let err = (self.status & 0x04) != 0;
        let write_done_pulse = !self.op_write && self.last_cmd == 0x02 && self.irq_countdown > 0;
        let mut on = self.irq_enable && (drdy || err || write_done_pulse);
        if on {
            self.irq_countdown = self.irq_hold_cycles;
        } else if self.irq_countdown > 0 {
            self.irq_countdown -= 1;
            on = true;
        }
        if self.use_firq {
            (false, on, false)
        } else {
            (false, false, on)
        }
    }
}

// ------------------------------------------------------------
// Simple GPIO device with OUT/IN/DIR registers
// Registers (base..base+3):
//  +0: OUT (R/W) output latch
//  +1: IN  (R)  input state (for inputs reads in_mask; for outputs reads OUT)
//  +2: DIR (R/W) 1=output, 0=input
//  +3: CTRL (W)  bit0: clear inputs; (R) not used
pub struct GpioDev {
    pub base: u16,
    pub bits: u8,
    out: u32,
    dir: u32,
    in_mask: u32,
}

impl GpioDev {
    pub fn new(base: u16, bits: u8) -> Self {
        Self {
            base,
            bits: bits.min(32),
            out: 0,
            dir: 0xFFFF_FFFF,
            in_mask: 0,
        }
    }
    pub fn get_state(&self) -> (u32, u32, u8) {
        (self.out, self.dir, self.bits)
    }
    pub fn set_out(&mut self, mask: u32) {
        self.out = mask & Self::mask_for(self.bits);
    }
    pub fn set_dir(&mut self, mask: u32) {
        self.dir = mask & Self::mask_for(self.bits);
    }
    pub fn set_inputs(&mut self, mask: u32) {
        self.in_mask = mask & Self::mask_for(self.bits);
    }
    fn mask_for(bits: u8) -> u32 {
        if bits >= 32 {
            0xFFFF_FFFF
        } else {
            (1u32 << bits) - 1
        }
    }
}

impl Device for GpioDev {
    fn contains(&self, addr: u16) -> bool {
        let span = ((self.bits as u32 + 7) / 8).max(1) * 4;
        (addr.wrapping_sub(self.base) as u32) < span
    }
    fn read8(&mut self, addr: u16) -> u8 {
        let off = addr.wrapping_sub(self.base) as usize;
        let group = off / 4;
        let sub = off % 4;
        let slots = ((self.bits as usize + 7) / 8).max(1);
        if group >= slots {
            return 0xFF;
        }
        let shift = (group * 8) as u32;
        match sub {
            0 => ((self.out >> shift) & 0xFF) as u8,
            1 => {
                let mask = Self::mask_for(self.bits);
                let actual = (self.out & self.dir) | (self.in_mask & !self.dir);
                ((actual & mask) >> shift & 0xFF) as u8
            }
            2 => ((self.dir >> shift) & 0xFF) as u8,
            3 => 0x00,
            _ => 0xFF,
        }
    }
    fn write8(&mut self, addr: u16, data: u8) {
        let off = addr.wrapping_sub(self.base) as usize;
        let group = off / 4;
        let sub = off % 4;
        let slots = ((self.bits as usize + 7) / 8).max(1);
        if group >= slots {
            return;
        }
        let shift = (group * 8) as u32;
        let byte_mask = 0xFFu32 << shift;
        let full_mask = Self::mask_for(self.bits);
        match sub {
            0 => {
                let new_out = (self.out & !byte_mask) | ((data as u32) << shift);
                self.out = new_out & full_mask;
                if GPIO_LOG.load(Ordering::SeqCst) {
                    eprintln!(
                        "[gpio] OUT @${:04X}+{} <= ${:02X}",
                        self.base,
                        group * 4,
                        data
                    );
                }
                publish_gpio_broadcast(true, self.bits, self.out, self.dir);
            }
            1 => { /* read-only */ }
            2 => {
                let new_dir = (self.dir & !byte_mask) | ((data as u32) << shift);
                self.dir = new_dir & full_mask;
                publish_gpio_broadcast(true, self.bits, self.out, self.dir);
            }
            3 => {
                if group == 0 && (data & 0x01) != 0 {
                    self.in_mask = 0;
                }
            }
            _ => {}
        }
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// Debug logging toggle for GPIO writes
static GPIO_LOG: AtomicBool = AtomicBool::new(false);
pub fn set_gpio_log(on: bool) {
    GPIO_LOG.store(on, Ordering::SeqCst);
}

// Cross-thread GPIO snapshot broadcast (for UI pickup even if UiSetGpio drops)
static GPIO_BROADCAST: OnceCell<Mutex<Option<(bool, u8, u32, u32)>>> = OnceCell::new();
pub fn publish_gpio_broadcast(present: bool, bits: u8, out: u32, dir: u32) {
    if let Some(m) = GPIO_BROADCAST.get() {
        if let Ok(mut g) = m.lock() {
            *g = Some((present, bits, out, dir));
        }
    } else {
        let _ = GPIO_BROADCAST.set(Mutex::new(Some((present, bits, out, dir))));
    }
}
pub fn take_gpio_broadcast() -> Option<(bool, u8, u32, u32)> {
    GPIO_BROADCAST
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|mut g| g.take())
}

// Non-consuming read of the latest snapshot (UI can poll every frame)
pub fn peek_gpio_broadcast() -> Option<(bool, u8, u32, u32)> {
    GPIO_BROADCAST
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|g| (*g).clone())
}

pub struct IoBus<B: Bus> {
    inner: B,
    devices: Vec<Box<dyn Device>>,
    gpio_dirty: bool,
    mem_watch_start: u16,
    mem_watch_len: usize,
    mem_dirty: bool,
}

impl<B: Bus> IoBus<B> {
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            devices: Vec::new(),
            gpio_dirty: false,
            mem_watch_start: 0,
            mem_watch_len: 0,
            mem_dirty: false,
        }
    }
    pub fn add_device<D: Device + 'static>(&mut self, dev: D) {
        self.devices.push(Box::new(dev));
    }
    pub fn feed_console_input(&mut self, bytes: &[u8]) {
        for d in self.devices.iter_mut() {
            if let Some(c) = d.as_any_mut().downcast_mut::<Mc6850Dev>() {
                c.feed_bytes(bytes);
                return;
            }
        }
    }
    /// Run `f` against the attached console device, if any.  Returns
    /// `true` when the closure ran (a console was attached) and
    /// `false` when no console is attached on this bus — letting
    /// callers (notably the boot-script runner) detect the
    /// silent-no-op case where an Action targets a device that
    /// configuration left unattached.  See
    /// `docs/en/config_and_boot_script.md` for the rationale.
    pub fn with_console_mut<F: FnOnce(&mut Mc6850Dev)>(&mut self, f: F) -> bool {
        for d in self.devices.iter_mut() {
            if let Some(c) = d.as_any_mut().downcast_mut::<Mc6850Dev>() {
                f(c);
                return true;
            }
        }
        false
    }
    /// Run `f` against the attached block device, if any.  Same
    /// detection semantics as [`Self::with_console_mut`].
    pub fn with_block_mut<F: FnOnce(&mut BlockDev)>(&mut self, f: F) -> bool {
        for d in self.devices.iter_mut() {
            if let Some(b) = d.as_any_mut().downcast_mut::<BlockDev>() {
                f(b);
                return true;
            }
        }
        false
    }
    pub fn inner_any_mut(&mut self) -> &mut dyn Any {
        self.inner.as_any_mut()
    }
    pub fn with_gpio_mut<F: FnOnce(&mut GpioDev)>(&mut self, f: F) {
        for d in self.devices.iter_mut() {
            if let Some(g) = d.as_any_mut().downcast_mut::<GpioDev>() {
                f(g);
                return;
            }
        }
    }
    pub fn take_gpio_dirty(&mut self) -> bool {
        if self.gpio_dirty {
            self.gpio_dirty = false;
            true
        } else {
            false
        }
    }
    pub fn set_mem_watch(&mut self, start: u16, len: usize) {
        self.mem_watch_start = start;
        self.mem_watch_len = len;
    }
    pub fn take_mem_dirty(&mut self) -> bool {
        if self.mem_dirty {
            self.mem_dirty = false;
            true
        } else {
            false
        }
    }
    pub fn take_block_dirty(&mut self) -> bool {
        for d in self.devices.iter_mut() {
            if let Some(b) = d.as_any_mut().downcast_mut::<BlockDev>() {
                if b.take_dirty() {
                    return true;
                }
            }
        }
        false
    }

    // Dynamic device management helpers (used by Settings Apply).
    /// Ensure exactly one console device (`Mc6850Dev`) exists at `base`,
    /// or none if `enable` is false.  Any existing console device at a
    /// different base is removed and replaced.
    pub fn ensure_console(&mut self, enable: bool, base: u16) {
        let has_mc6850 = self
            .devices
            .iter_mut()
            .any(|d| d.as_any_mut().downcast_mut::<Mc6850Dev>().is_some());
        if CONSOLE_LOG.load(Ordering::SeqCst) {
            eprintln!("[console-swap] enable={enable} base=${base:04X} has_mc6850={has_mc6850}");
        }
        if !enable && has_mc6850 {
            let mut keep = Vec::with_capacity(self.devices.len());
            for mut dev in self.devices.drain(..) {
                if dev.as_any_mut().downcast_mut::<Mc6850Dev>().is_none() {
                    keep.push(dev);
                }
            }
            self.devices = keep;
        }
        if enable && !has_mc6850 {
            self.add_device(Mc6850Dev::new(base));
        }
    }
    pub fn with_console_mut_opt<F: FnOnce(&mut Mc6850Dev)>(&mut self, f: F) {
        for d in self.devices.iter_mut() {
            if let Some(c) = d.as_any_mut().downcast_mut::<Mc6850Dev>() {
                f(c);
                return;
            }
        }
    }
    pub fn ensure_block(&mut self, enable: bool, base: u16) {
        let mut has = false;
        for d in self.devices.iter_mut() {
            if d.as_any_mut().downcast_mut::<BlockDev>().is_some() {
                has = true;
            }
        }
        if enable {
            if !has {
                self.add_device(BlockDev::new(base));
            }
        } else if has {
            let mut new_list = Vec::with_capacity(self.devices.len());
            for mut dev in self.devices.drain(..) {
                let is_blk = dev.as_any_mut().downcast_mut::<BlockDev>().is_some();
                if !is_blk {
                    new_list.push(dev);
                }
            }
            self.devices = new_list;
        }
    }
    pub fn with_block_mut_opt<F: FnOnce(&mut BlockDev)>(&mut self, f: F) {
        for d in self.devices.iter_mut() {
            if let Some(b) = d.as_any_mut().downcast_mut::<BlockDev>() {
                f(b);
                return;
            }
        }
    }
    pub fn ensure_gpio(&mut self, enable: bool, base: u16, bits: u8) {
        let mut has = false;
        for d in self.devices.iter_mut() {
            if d.as_any_mut().downcast_mut::<GpioDev>().is_some() {
                has = true;
            }
        }
        if enable {
            if !has {
                self.add_device(GpioDev::new(base, bits));
            } else {
                // Update existing GPIO device's base/bits and clamp state to new mask
                let new_bits = bits.min(32);
                let mask: u32 = if new_bits >= 32 {
                    0xFFFF_FFFF
                } else {
                    (1u32 << new_bits) - 1
                };
                for d in self.devices.iter_mut() {
                    if let Some(g) = d.as_any_mut().downcast_mut::<GpioDev>() {
                        g.base = base;
                        g.bits = new_bits;
                        // Clamp current latches to new bit width
                        g.out &= mask;
                        g.dir &= mask;
                        g.in_mask &= mask;
                        break;
                    }
                }
            }
        } else if has {
            let mut new_list = Vec::with_capacity(self.devices.len());
            for mut dev in self.devices.drain(..) {
                let is_gpio = dev.as_any_mut().downcast_mut::<GpioDev>().is_some();
                if !is_gpio {
                    new_list.push(dev);
                }
            }
            self.devices = new_list;
        }
    }
    pub fn with_gpio_mut_opt<F: FnOnce(&mut GpioDev)>(&mut self, f: F) {
        for d in self.devices.iter_mut() {
            if let Some(g) = d.as_any_mut().downcast_mut::<GpioDev>() {
                f(g);
                return;
            }
        }
    }
    pub fn ensure_timer(&mut self, enable: bool, base: u16) {
        let mut has = false;
        for d in self.devices.iter_mut() {
            if d.as_any_mut()
                .downcast_mut::<crate::timer::TimerDev>()
                .is_some()
            {
                has = true;
            }
        }
        if enable {
            if !has {
                self.add_device(crate::timer::TimerDev::new(base));
            }
        } else if has {
            let mut new_list = Vec::with_capacity(self.devices.len());
            for mut dev in self.devices.drain(..) {
                let is_t = dev
                    .as_any_mut()
                    .downcast_mut::<crate::timer::TimerDev>()
                    .is_some();
                if !is_t {
                    new_list.push(dev);
                }
            }
            self.devices = new_list;
        }
    }
    pub fn with_timer_mut_opt<F: FnOnce(&mut crate::timer::TimerDev)>(&mut self, f: F) {
        for d in self.devices.iter_mut() {
            if let Some(t) = d.as_any_mut().downcast_mut::<crate::timer::TimerDev>() {
                f(t);
                return;
            }
        }
    }

    pub fn with_timer_mut<F: FnOnce(&mut crate::timer::TimerDev)>(&mut self, f: F) {
        for d in self.devices.iter_mut() {
            if let Some(t) = d.as_any_mut().downcast_mut::<crate::timer::TimerDev>() {
                f(t);
                return;
            }
        }
    }
}

impl<B: Bus + 'static> Bus for IoBus<B> {
    fn read8(&mut self, addr: u16) -> u8 {
        for d in self.devices.iter_mut() {
            if d.contains(addr) {
                return d.read8(addr);
            }
        }
        self.inner.read8(addr)
    }
    fn write8(&mut self, addr: u16, data: u8) {
        for d in self.devices.iter_mut() {
            if d.contains(addr) {
                if let Some(g) = d.as_any_mut().downcast_mut::<GpioDev>() {
                    let off = addr.wrapping_sub(g.base);
                    if off == 0 || off == 2 {
                        self.gpio_dirty = true;
                    }
                }
                // mark mem dirty if falls within watch
                if self.mem_watch_len > 0 {
                    let s = self.mem_watch_start;
                    let e = s.wrapping_add(self.mem_watch_len as u16);
                    let in_range = if s <= e {
                        addr >= s && addr < e
                    } else {
                        addr >= s || addr < e
                    };
                    if in_range {
                        self.mem_dirty = true;
                    }
                }
                d.write8(addr, data);
                return;
            }
        }
        // mark mem dirty on inner writes too
        if self.mem_watch_len > 0 {
            let s = self.mem_watch_start;
            let e = s.wrapping_add(self.mem_watch_len as u16);
            let in_range = if s <= e {
                addr >= s && addr < e
            } else {
                addr >= s || addr < e
            };
            if in_range {
                self.mem_dirty = true;
            }
        }
        self.inner.write8(addr, data)
    }
    fn irq_lines(&mut self) -> (bool, bool, bool) {
        let mut nmi = false;
        let mut firq = false;
        let mut irq = false;
        for d in self.devices.iter_mut() {
            let (n, f, i) = d.irq_lines();
            nmi |= n;
            firq |= f;
            irq |= i;
        }
        let (n2, f2, i2) = self.inner.irq_lines();
        (nmi | n2, firq | f2, irq | i2)
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
