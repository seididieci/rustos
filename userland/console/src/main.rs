//! Console server (Fase 8.2 + 9.4 + 15.4): driver VGA/rendering puro.
//!
//! E' l'UNICO processo che disegna sul frame buffer VGA e pubblica il device
//! di output `/dev/console` (DEV_WRITE disegna i byte). La tastiera vive
//! altrove (Fase 15): `userkbd` pubblica scancode raw su `/dev/kbd`, `usertty`
//! decodifica, fa echo scrivendo qui e serve i byte cotti su
//! `/dev/input/keyboard`.
//!
//! Il cursore hardware VGA (CRTC 0x3D4/0x3D5) segue il punto di scrittura.

#![no_std]
#![no_main]

extern crate alloc;
use libr;

/// Costante copiata da `vmm_user.rs` (il crate kernel non e' linkato qui).
const USER_VGA: u64 = 0x4000_0010_0000;

/// Indirizzo fisico del frame buffer VGA.
const VGA_PHYS: u64 = 0xB8000;

/// Dimensioni del buffer VGA (testo 80x25).
const VGA_ROWS: usize = 25;
const VGA_COLS: usize = 80;

/// Porte CRTC per il cursore hardware VGA.
const CRTC_INDEX: u16 = 0x3D4;
const CRTC_DATA: u16 = 0x3D5;

// ── IPC tags ────────────────────────────────────────────────────────

/// IPC tags verso userfs (devono combaciare con userfs/devfs/tty).
const DEV_OPEN: u64 = 0x20;
const DEV_READ: u64 = 0x21;
const DEV_WRITE: u64 = 0x22;
const DEV_CLOSE: u64 = 0x23;

/// Device type per DEV_OPEN (output console, vedi `dev_type` in userfs).
const DEV_CONSOLE: u64 = 3;

/// Valore di errore IPC.
const ERR: u64 = !0u64;

// ── VGA buffer ───────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct ScreenChar {
    ascii: u8,
    color: u8,
}

#[repr(C)]
struct Buffer {
    chars: [[ScreenChar; VGA_COLS]; VGA_ROWS],
}

/// Colore unico del terminale (bianco su nero): un solo writer, colori coerenti.
const COLOR: u8 = 0x0F;

/// Porta di output 8-bit (ring 3 consentito via TSS I/O bitmap, ADR-0006).
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nostack, nomem));
    }
}

/// Sposta il cursore hardware VGA alla cella (row, col).
unsafe fn move_hw_cursor(row: usize, col: usize) {
    let offset = (row * VGA_COLS + col) as u16;
    unsafe {
        outb(CRTC_INDEX, 0x0E);
        outb(CRTC_DATA, (offset >> 8) as u8);
        outb(CRTC_INDEX, 0x0F);
        outb(CRTC_DATA, (offset & 0xFF) as u8);
    }
}

/// Scrive un singolo byte sul frame buffer VGA a `USER_VGA`.
/// Safety: `vga` deve puntare a un buffer VGA valido mappato in questo
/// processo.
unsafe fn vga_write_byte(vga: *mut Buffer, row: usize, col: usize, byte: u8) {
    let ch = ScreenChar { ascii: byte, color: COLOR };
    unsafe {
        core::ptr::write_volatile(&mut (*vga).chars[row][col], ch);
    }
}

/// Pulisce una riga intera.
unsafe fn vga_clear_row(vga: *mut Buffer, row: usize) {
    for col in 0..VGA_COLS {
        unsafe { vga_write_byte(vga, row, col, b' ') };
    }
}

/// Scorrimento: copia le righe 1..24 verso l'alto e pulisce l'ultima.
unsafe fn vga_scroll(vga: *mut Buffer) {
    for row in 1..VGA_ROWS {
        for col in 0..VGA_COLS {
            let ch = unsafe { core::ptr::read_volatile(&(*vga).chars[row][col]) };
            unsafe { core::ptr::write_volatile(&mut (*vga).chars[row - 1][col], ch) };
        }
    }
    unsafe { vga_clear_row(vga, VGA_ROWS - 1) };
}

/// Scrive un carattere ASCII sul VGA (ultima riga) con scroll automatico.
/// Aggiorna il cursore software e sposta il cursore hardware a seguire.
unsafe fn vga_write_char(vga: *mut Buffer, byte: u8, cursor: &mut usize) {
    match byte {
        b'\x0c' => {
            // Form feed (Fase 18.1, builtin `clear`): pulisci tutto e home.
            for row in 0..VGA_ROWS {
                unsafe { vga_clear_row(vga, row) };
            }
            *cursor = 0;
        }
        b'\n' => {
            unsafe { vga_scroll(vga) };
            *cursor = 0;
        }
        b'\r' => *cursor = 0,
        b'\x08' => {
            if *cursor > 0 {
                *cursor -= 1;
                unsafe { vga_write_byte(vga, VGA_ROWS - 1, *cursor, b' ') };
            }
        }
        0x20..=0x7e => {
            let row = VGA_ROWS - 1;
            let col = *cursor;
            unsafe { vga_write_byte(vga, row, col, byte) };
            *cursor += 1;
            if *cursor >= VGA_COLS {
                unsafe { vga_scroll(vga) };
                *cursor = 0;
            }
        }
        _ => {}
    }
    unsafe { move_hw_cursor(VGA_ROWS - 1, *cursor) };
}

// ── Ring I/O (Fase 10.2) ─────────────────────────────────────────
// Le finestre CLI_* sono mappate da userfs (map_in) con i ring del client
// a ogni relay DEV (zero-copy); i ring propri del server non cambiano mai.

const REQ_RING_VA: u64 = libr::CLI_REQ_VA;
const RESP_RING_VA: u64 = libr::CLI_RESP_VA;
const RING_DATA_CAP: usize = 4088;
const RING_HEAD: usize = 0xFF8;
const RING_TAIL: usize = 0xFFC;

unsafe fn ring_positions(ring_va: u64) -> (u32, u32) {
    let head = unsafe { core::ptr::read_volatile((ring_va + RING_HEAD as u64) as *const u32) };
    let tail = unsafe { core::ptr::read_volatile((ring_va + RING_TAIL as u64) as *const u32) };
    (head, tail)
}

/// Scrive dati nella response ring del client (a RESP_RING_VA).
unsafe fn resp_ring_write_client(data: &[u8]) {
    let frame_len = 16 + data.len();
    unsafe {
        let (head, _tail) = ring_positions(RESP_RING_VA);
        let mut hdr = [0u8; 16];
        hdr[0..8].copy_from_slice(&(data.len() as u64).to_le_bytes());
        hdr[8..16].copy_from_slice(&0u64.to_le_bytes());
        let dst = RESP_RING_VA as *mut u8;
        for (i, byte) in hdr.iter().enumerate() {
            let p = ((head as usize) + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        for (i, byte) in data.iter().enumerate() {
            let p = ((head as usize) + 16 + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((RESP_RING_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// Legge `count` byte di payload dalla request ring del client (a REQ_RING_VA)
/// e avanza la tail di (20 + count): il frame request e' [tag:4][fd:8][count:8]
/// seguito da `count` byte di dati. Ritorna il numero di byte letti.
unsafe fn req_ring_read_client(dst: &mut [u8], count: usize) -> usize {
    unsafe {
        let (_head, tail) = ring_positions(REQ_RING_VA);
        let src = REQ_RING_VA as *const u8;
        let n = count.min(dst.len());
        for i in 0..n {
            let p = ((tail as usize) + 20 + i) % RING_DATA_CAP; // salta header frame (20 B)
            dst[i] = core::ptr::read_volatile(src.add(p));
        }
        // Consuma il frame (header + payload): la tail deve avanzare o il ring
        // resta pieno e il client non puo' piu' scrivere.
        let new_tail = ((tail as usize) + 20 + n) % RING_DATA_CAP;
        core::ptr::write_volatile((REQ_RING_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
        n
    }
}

// ── Entry point ──────────────────────────────────────────────────────

/// Assicura il mount "/dev/console" presso userfs (Fase 14, t28 + Fase 15):
/// attende Fs via soli lookup, poi UN tentativo (vedi corpo). Stessa funzione
/// a boot e su EXIT_NOTIFY. Unbounded come `fs_chan`. Idempotente grazie al
/// replace-on-register in userfs.
fn ensure_mounted() {
    // Prima i PROPRI ring (vedi devfs: map_in altrui li sovrascrive).
    let _ = libr::fs_remap_self();
    // Attende Fs via soli lookup (NESSUN frame scritto finche' userfs non
    // c'e': niente spam nel ring che disallineerebbe gli altri client), poi
    // UN tentativo; se fallisce ricomincia dal lookup. Stessa funzione a boot
    // e su EXIT_NOTIFY (t28). Unbounded come `fs_chan`.
    loop {
        while libr::service_lookup(libr::Service::Fs).is_err() {
            // Attesa in solo spin (IF=1): niente busy-loop su `get_ticks`, che
            // terrebbe gli interrupt mascherati dentro le syscall.
            for _ in 0..1_000_000 {
                core::hint::spin_loop();
            }
        }
        if libr::fs_register(b"/dev/console") == 0 {
            return;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // 1. Mappa il frame buffer VGA.
    let _ = libr::map_physical(VGA_PHYS, USER_VGA, 1);
    let vga = USER_VGA as *mut Buffer;

    // 2. Pulisci il VGA.
    for row in 0..VGA_ROWS {
        unsafe { vga_clear_row(vga, row) };
    }

    // 3. Stampa banner (il terminale parte in fondo, come un prompt).
    let msg = b"Velordor console server";
    let mut cursor = 0usize;
    for &b in msg {
        unsafe { vga_write_char(vga, b, &mut cursor) };
    }
    let _ = libr::print_string(b"[console] server up\n");

    // 4b. Registra il servizio Console per nome (ADR-0008): init lo usa per
    // la supervisione (service_pid) e i client potrebbero risolverlo.
    if libr::service_register(libr::Service::Console).is_ok() {
        let _ = libr::print_string(b"[console] registered as service Console\n");
    }

    // 4c. Avvisa il parent (init) di essere pronto (SVC_READY fire-and-forget):
    // serve al supervisore init-restart (Fase 14). SUBITO dopo la registrazione
    // del servizio (non dopo /dev/console, che richiede userfs non ancora nato:
    // init aspetta questo ack a boot e attendere dopo sarebbe deadlock).
    // Fire-and-forget (send_async): retry bounded, mai hang.
    for _ in 0..100 {
        if libr::send_async(libr::CHANNEL_PARENT, 0x7D, 1, 0).is_ok() {
            break;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }

    // 5. Registra /dev/console con userfs (IPC FS_REGISTER via libr::fs_register,
    //    che prima alloca e registra la pagina FS per-processo).
    //    ensure_mounted: stessa funzione a boot e su EXIT_NOTIFY (t28).
    ensure_mounted();
    let _ = libr::print_string(b"[console] registered /dev/console with userfs\n");

    // 6. Loop IPC: solo richieste DEV sul device di output (+ EXIT_NOTIFY).
    //    Niente piu' tastiera qui (Fase 15: userkbd + usertty).
    loop {
        match libr::recv() {
            Ok(msg) => {
                match msg.tag {
                    DEV_OPEN => {
                        // msg.w0 = device type
                        if msg.w0 == DEV_CONSOLE {
                            let _ = libr::reply(msg.tag, 0, 0);
                        } else {
                            let _ = libr::reply(msg.tag, ERR, 0);
                        }
                    }

                    DEV_READ => {
                        // Output-only: EOF immediato (frame vuoto + 0), come
                        // /dev/null. Frame SEMPRE (anche vuoto): il client
                        // distingue "0 byte" da "ring vuoto" solo dal frame.
                        unsafe { resp_ring_write_client(&[]); }
                        let _ = libr::reply(msg.tag, 0, 0);
                    }

                    DEV_WRITE => {
                        // msg.w0 = fd, msg.w1 = count. I byte da disegnare sono
                        // nella request ring del client (mappata da userfs via map_in).
                        let count = msg.w1 as usize;
                        if count > 0 {
                            let mut data = alloc::vec::Vec::with_capacity(count);
                            data.resize(count, 0);
                            unsafe { req_ring_read_client(&mut data, count); }
                            for b in &data {
                                unsafe { vga_write_char(vga, *b, &mut cursor) };
                            }
                        }
                        let _ = libr::reply(msg.tag, msg.w1, 0);
                    }

                    DEV_CLOSE => {
                        let _ = libr::reply(msg.tag, 0, 0);
                    }

                    libr::EXIT_NOTIFY => {
                        // userfs morto e rinato (t28): re-mount. Nessuno stato
                        // per-client da purgare; mai rispondere alle notifiche.
                        ensure_mounted();
                    }

                    _ => {
                        let _ = libr::reply(msg.tag, 0, 0);
                    }
                }
            }
            Err(()) => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    let _ = libr::print_string(b"[console] panic\n");
    libr::exit(1)
}
