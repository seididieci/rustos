//! userkbd — Driver tastiera PS/2 in userspace (Fase 15).
//!
//! Possiede le porte 0x60/0x64 (via `io_ranges`, TSS per-processo ADR-0006) e
//! pubblica gli scancode raw (Set 1) sul device `/dev/kbd`, registrato presso
//! userfs come gli altri driver (FS_REGISTER). La decodifica resta fuori: sara'
//! `usertty` a leggere `/dev/kbd` e a servire i byte cotti (Fase 15.3).
//!
//! Risveglio: il kernel su IRQ1 fa solo routing + EOI e sveglia l'owner del
//! servizio `Kbd` per nome. Il loop e' un `recv` bloccante: il wake IRQ lo
//! rende Ready (via `pending_wake` se non ancora bloccato) e `recv` ritorna
//! `Err` sullo wake spurio — in ogni giro si drena l'hardware (bit OBF di
//! 0x64) prima di servire i messaggi. Nessun tasto perso per race wake/read:
//! un byte arrivato mentre giriamo alza un nuovo IRQ che ci risveglia di nuovo.

#![no_std]
#![no_main]

extern crate alloc;

// Port I/O (A4): single source in `libr::pio` (i call site `io::*` restano).
use libr::pio as io;

use libr::println;

// ── IPC tags da userfs ──────────────────────────────────────────────

// ── IPC tags ────────────────────────────────────────────────────────
// DEV_* come gli altri driver (da userfs). KBD_NOTIFY e' diverso: e' kbd che
// avvisa tty "ci sono scancode" (fire-and-forget, NESSUNA reply: il mittente
// async non aspetta). Senza notify tty dovrebbe pompare in polling (sempre
// Ready → dilution dello scheduler, vedi diagnosi t30 Fase 15).

// ── IPC tags + device type (DocsD: single source in `syscall-numbers`) ─
use libr::{DEV_CLOSE, DEV_KBD, DEV_OPEN, DEV_READ, DEV_READDIR, DEV_WRITE};

/// Notify a tty: scancode in attesa (w1 = quanti, hint).
/// Single source in `syscall-numbers` (DocsB), via `libr`.
use libr::KBD_NOTIFY;

// ── Ring I/O (Fase 10.2, stesso pattern di devfs) ───────────────────
// La response del client va nella finestra CLI_RESP_VA (mappata da userfs
// con i ring del client a ogni relay DEV).

const RESP_RING_VA: u64 = libr::CLI_RESP_VA;
// Geometria ring + errore IPC (A1): single source in `libr` (ERR ancora usato).
use libr::ERR;

// ── Porte PS/2 ──────────────────────────────────────────────────────

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_OBF: u8 = 0x01;
const PS2_IBF: u8 = 0x02;
/// Bit 5 di 0x64: il byte in attesa viene dal mouse (AUX), non dalla tastiera.
const PS2_AUX: u8 = 0x20;
/// Bit di errore di 0x64: parita' (7) e timeout (6) — il byte e' spazzatura.
const PS2_ERR: u8 = 0xC0;

// ── Coda scancode interna ───────────────────────────────────────────
// Come la vecchia coda kernel (`kbd_events`, ora rimossa): cap 256, i byte in
// eccesso sotto raffica vengono scartati (stesso contratto di prima).

const SCANCAP: usize = 256;

struct ScanQueue {
    buf: [u8; SCANCAP],
    head: usize,
    tail: usize,
    len: usize,
}

impl ScanQueue {
    const fn new() -> Self {
        Self { buf: [0; SCANCAP], head: 0, tail: 0, len: 0 }
    }

    fn push(&mut self, sc: u8) {
        if self.len == SCANCAP {
            return;
        }
        self.buf[self.tail] = sc;
        self.tail = (self.tail + 1) % SCANCAP;
        self.len += 1;
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Drena fino a `out.len()` byte, ritorna quanti.
    fn drain_into(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        while self.len > 0 && n < out.len() {
            out[n] = self.buf[self.head];
            self.head = (self.head + 1) % SCANCAP;
            self.len -= 1;
            n += 1;
        }
        n
    }
}

/// Attende IBF libero (controller pronto a ricevere un comando) con bound.
/// Senza, un comando scritto mentre il controller e' occupato va perso
/// (es. 0xA7 che dovrebbe disabilitare il mouse).
fn wait_ibf_clear() {
    for _ in 0..100_000 {
        if unsafe { io::inb(PS2_STATUS) } & PS2_IBF == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Attende OBF alto (byte in arrivo) con bound. Ritorna false a timeout.
fn wait_obf_set() -> bool {
    for _ in 0..100_000 {
        if unsafe { io::inb(PS2_STATUS) } & PS2_OBF != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Inizializzazione controller i8042 (spostata dal kernel, `keyboard.rs`:
/// Fase 15 vuole tutto in userland). Senza, IRQ1 non viene generato neanche
/// con la maschera PIC sbloccata (tipico in PVH mode).
fn i8042_init() {
    unsafe {
        // Drain di backlog (SeaBIOS / mouse pre-init): scarta tutto.
        loop {
            let st = io::inb(PS2_STATUS);
            if st & PS2_OBF == 0 {
                break;
            }
            let _ = io::inb(PS2_DATA);
        }

        // Disabilita keyboard + mouse durante la config (con IBF-wait: senza,
        // il comando puo' andare perso e il mouse resta attivo a sporcare OBF).
        wait_ibf_clear();
        io::outb(PS2_STATUS, 0xAD);
        wait_ibf_clear();
        io::outb(PS2_STATUS, 0xA7);

        // Leggi command byte, imposta bit 0 (IRQ1 enable), riscrivi.
        wait_ibf_clear();
        io::outb(PS2_STATUS, 0x20);
        let cmd: u8 = if wait_obf_set() {
            io::inb(PS2_DATA)
        } else {
            0x65
        };
        wait_ibf_clear();
        io::outb(PS2_STATUS, 0x60);
        wait_ibf_clear();
        io::outb(PS2_DATA, cmd | 0x01);

        // Riabilita tastiera + abilita scanning.
        wait_ibf_clear();
        io::outb(PS2_STATUS, 0xAE);
        wait_ibf_clear();
        io::outb(PS2_DATA, 0xF4);

        // ACK con timeout (niente lettura cieca: senza OBF si leggerebbe
        // spazzatura che finirebbe decodificata come tasto).
        if wait_obf_set() {
            let _ = io::inb(PS2_DATA);
        }
    }
    println!("[userkbd] i8042 init (IRQ1 abilitata)");
}

/// Drena l'hardware: finche' OBF e' alto, leggi uno scancode e accodalo.
/// I byte AUX (mouse) o con errori di parita'/timeout vengono LETTI (per
/// abbassare OBF, altrimenti il buffer HW si riempie e si perdono tasti veri)
/// ma SCARTATI, mai accodati: decodificarli come tasti corrompe lo stream
/// (osservato: caratteri sbagliati intermittenti in GTK con mouse vivo).
/// Chiamato a ogni giro di loop (dopo recv o wake spurio): tra IRQ e drain
/// non si perde nulla, e un byte arrivato durante il drain alza un nuovo IRQ.
fn drain_hw(q: &mut ScanQueue) {
    unsafe {
        loop {
            let st = io::inb(PS2_STATUS);
            if st & PS2_OBF == 0 {
                break;
            }
            let b = io::inb(PS2_DATA);
            if st & (PS2_AUX | PS2_ERR) != 0 {
                continue;
            }
            q.push(b);
        }
    }
}

// Frame helper response (A2): single source in `libr` (prima identica qui).
use libr::resp_frame_write;

/// Assicura il mount "/dev/kbd" presso userfs (stesso pattern di devfs,
/// `ensure_mounted`): attende Fs via soli lookup, poi UN tentativo; se
/// fallisce ricomincia. Unbounded: senza Fs il driver e' comunque inutile.
fn ensure_mounted() {
    libr::ensure_fs_mount(|| libr::fs_register(b"/dev/kbd"));
}

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    println!("[userkbd] starting, pid={}", libr::getpid());

    // Hardware prima di tutto: da qui in poi gli IRQ1 arrivano e il kernel ci
    // sveglia (il servizio non e' ancora registrato: i wake vanno persi ma
    // nessun byte resta nel controller — il primo drain_hw li raccoglie).
    i8042_init();

    // Registra il servizio Kbd per nome (ADR-0008): il kernel risolve l'owner
    // su IRQ1 per il wake.
    if libr::service_register(libr::Service::Kbd).is_ok() {
        println!("[userkbd] registered as service Kbd");
    }

    // Registra il prefix "/dev/kbd" presso userfs.
    ensure_mounted();
    println!("[userkbd] registered /dev/kbd with userfs");

    // Avvisa il parent (init) di essere pronto (SVC_READY fire-and-forget,
    // come devfs: a boot init aspetta, su restart nessuno — mai sync).
    libr::signal_ready(1);

    let mut queue = ScanQueue::new();
    let mut next_fd: u32 = 1;
    // Canale verso tty per KBD_NOTIFY (lookup pigro + re-lookup se tty muore).
    let mut tty_chan: i64 = -1;
    let mut last_notify_tick: i64 = 0;

    loop {
        // Wake IRQ o messaggio: in ogni caso prima drena l'hardware (vedi
        // doc in testa). `recv` su wake spurio ritorna Err: nessun problema,
        // il drain e' comunque avvenuto.
        match libr::recv() {
            Ok(m) => {
                // userfs morto e rinato: re-mount (come devfs, t28). Mai reply.
                if m.tag == libr::EXIT_NOTIFY {
                    println!("[userkbd] peer morto, re-mount /dev/kbd");
                    ensure_mounted();
                    drain_hw(&mut queue);
                    continue;
                }

                // Notify IRQ dal kernel (bridge interrupt→IPC, canale 0 senza
                // peer): NESSUNA reply (non c'e' nessuno ad aspettarla;
                // risponderla manderebbe spazzatura sul canale di nascita).
                // Niente `continue` qui: si cade nel drain_hw + notify comuni
                // sotto, altrimenti lo scancode resta nel controller e tty non
                // viene mai avvisata.
                if m.tag != libr::IRQ_NOTIFY_KBD {
                    let result: Option<u64> = match m.tag {
                        DEV_OPEN => {
                            if m.w0 == DEV_KBD {
                                let fd = next_fd;
                                next_fd += 1;
                                Some(fd as u64)
                            } else {
                                None
                            }
                        }
                        DEV_READ => {
                            // Consegna subito il disponibile (anche 0 con frame
                            // vuoto, pattern /dev/null). Il lettore (usertty,
                            // notify-driven) riprova alla prossima notify. Niente
                            // reply differite: le VA map_in verrebbero rimappate
                            // da altri nel mentre.
                            let count = (m.w1 as usize).min(256);
                            let mut buf = [0u8; 256];
                            let n = queue.drain_into(&mut buf[..count]);
                            unsafe { resp_frame_write(RESP_RING_VA, &buf[..n]); }
                            Some(n as u64)
                        }
                        DEV_WRITE => None,
                        DEV_CLOSE => Some(0),
                        DEV_READDIR => {
                            let entry = b"kbd\0";
                            unsafe { resp_frame_write(RESP_RING_VA, entry); }
                            Some(1)
                        }
                        _ => None,
                    };
                    let _ = libr::reply(0, result.unwrap_or(ERR), 0);
                }
            }
            Err(_) => {}
        }
        let had = queue.len();
        drain_hw(&mut queue);
        // Event-driven (Fase 15): se ci sono scancode in attesa, avvisa tty
        // (che dorme in recv) con fire-and-forget. Throttle 2 tick sui resend
        // (notify persa per coda piena: si riprova qui, mai polling dedicato).
        // Senza notify, tty dovrebbe pompare sempre → sempre Ready → dilution.
        if queue.len() > 0 {
            let now = libr::get_ticks();
            if had == 0 || now.wrapping_sub(last_notify_tick) >= 2 {
                if tty_chan < 0 {
                    tty_chan = libr::service_lookup(libr::Service::Tty)
                        .unwrap_or(-1);
                }
                if tty_chan >= 0 {
                    if libr::send_async(tty_chan as u64, KBD_NOTIFY, queue.len() as u64, 0).is_ok() {
                        last_notify_tick = now;
                    } else {
                        // tty riavviato (canale morto): re-lookup al prossimo giro.
                        tty_chan = -1;
                    }
                }
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[userkbd] panic");
    libr::exit(1)
}
