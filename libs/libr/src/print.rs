use super::*;

// ── Line buffer (flush-on-newline) ────────────────────────────────

const LINE_BUF_SIZE: usize = 1024;
static mut LINE_BUF: [u8; LINE_BUF_SIZE] = [0u8; LINE_BUF_SIZE];
static LINE_LEN: AtomicUsize = AtomicUsize::new(0);

/// Svuota il line buffer scrivendo il contenuto su stdout (fd 1) tramite
/// una singola syscall, poi resetta il buffer. Con redirect attivo (Fase 40,
/// `stdio::set_stdio`) va sul file invece che sul seriale, con fallback
/// seriale a errore (l'output non si perde mai).
pub fn flush() {
    let len = LINE_LEN.swap(0, Ordering::Relaxed);
    if len > 0 {
        let ptr = core::ptr::addr_of!(LINE_BUF) as *const u8;
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        if !crate::stdio::route_out(bytes) {
            sys::write(1, ptr, len);
        }
    }
}

/// Scrive una stringa nel line buffer senza flush. Se il buffer e' pieno,
/// viene svuotato prima di continuare.
pub fn print_str(s: &str) {
    for &byte in s.as_bytes() {
        push_byte(byte);
    }
}

/// Scrive un byte nel line buffer. Se e' `\n`, flush automatico.
fn push_byte(b: u8) {
    unsafe {
        let cur = LINE_LEN.load(Ordering::Relaxed);
        if cur >= LINE_BUF_SIZE {
            flush();
        }
        let buf_ptr = core::ptr::addr_of_mut!(LINE_BUF) as *mut u8;
        let cur = LINE_LEN.load(Ordering::Relaxed);
        *buf_ptr.add(cur) = b;
        LINE_LEN.store(cur + 1, Ordering::Relaxed);
        if b == b'\n' {
            flush();
        }
    }
}

/// Scrive dati binari nel line buffer. I byte vengono flushati
/// automaticamente quando il buffer e' pieno o quando si incontra `\n`.
pub fn write_raw(buf: *const u8, count: usize) {
    for i in 0..count {
        unsafe { push_byte(*buf.add(i)); }
    }
}

/// Implementazione `core::fmt::Write` per il line buffer.
struct LineWriter;

impl fmt::Write for LineWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        print_str(s);
        Ok(())
    }
}

/// Formatta args nel line buffer usando `core::fmt`.
pub fn print_fmt(args: fmt::Arguments) {
    use fmt::Write;
    let _ = LineWriter.write_fmt(args);
}

/// Macro per output su stdout (senza newline automatico).
/// Il contenuto viene bufferizzato; il flush avviene quando il buffer
/// incontra `\n` o quando si chiama `libr::flush()`.
///
/// ```ignore
/// print_str!("[test] value=42");
/// println!();
/// ```
///
/// Supporta anche `print_str!("[test] val={}", val)` grazie a `fmt::Arguments`.
#[macro_export]
macro_rules! print_str {
    ($($arg:tt)*) => {
        $crate::print_fmt(format_args!($($arg)*))
    };
}

/// Macro per output su stdout con newline finale e flush immediato.
///
/// ```ignore
/// println!("[test] hello world");
/// println!("[test] value={}", 42);
/// ```
#[macro_export]
macro_rules! println {
    () => { $crate::print_str!("\n"); };
    ($($arg:tt)*) => {
        $crate::print_str!("{}\n", format_args!($($arg)*))
    };
}
