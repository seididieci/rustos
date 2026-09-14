//! DEBUG FACILITY intenzionale (ADR-0005 §3): resta nel kernel, la console
//! vera vive nel console server userspace.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::{Lazy, Mutex};
use uart_16550::SerialPort;

pub static SERIAL1: Lazy<Mutex<SerialPort>> = Lazy::new(|| {
    let mut serial_port = unsafe { SerialPort::new(0x3F8) };
    serial_port.init();
    Mutex::new(serial_port)
});

/// Vero se il prossimo carattere da scrivere inizia una nuova riga.
/// Persiste tra chiamate `_print` (le righe parziali senza `\n` non devono
/// ricevere un nuovo timestamp). Accesso solo con interrupt disabilitati.
static AT_LINE_START: AtomicBool = AtomicBool::new(true);

/// Wrapper stile dmesg: inserisce `[<secondi>.<centesimi>] ` all'inizio di
/// ogni riga non vuota (sorgente: `pit::ticks()` a 100 Hz). Le righe vuote
/// restano pulite. Copre kernel E userland: l'output user passa da
/// `sys_write` -> `serial_println!` -> qui.
struct DmesgPort<'a> {
    port: &'a mut SerialPort,
}

impl fmt::Write for DmesgPort<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut at_start = AT_LINE_START.load(Ordering::Relaxed);
        for ch in s.chars() {
            if at_start && ch != '\n' {
                let t = crate::pit::ticks();
                write!(self.port, "[{}.{:02}] ", t / 100, t % 100)?;
                at_start = false;
            }
            let mut cbuf = [0u8; 4];
            self.port.write_str(ch.encode_utf8(&mut cbuf))?;
            if ch == '\n' {
                at_start = true;
            }
        }
        AT_LINE_START.store(at_start, Ordering::Relaxed);
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let mut guard = SERIAL1.lock();
        let mut w = DmesgPort { port: &mut guard };
        w.write_fmt(args).expect("Printing to serial failed");
    });
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
