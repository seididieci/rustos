//! usertty — Terminal server in userspace (Fase 15).
//!
//! Legge scancode raw (Set 1) da `/dev/kbd` (driver `userkbd`), li decodifica
//! con `pc_keyboard` (layout US), fa echo su `/dev/console` e serve i byte
//! cotti ai client sul device `/dev/input/keyboard` con protocollo
//! byte-identico a prima: la shell non cambia una riga (char-by-char,
//! `Enter→\n`, `Backspace→0x08`, resto filtrato).
//!
//! REGOLA ANTI-DEADLOCK (Fase 15, ciclo userfs<->tty): un driver che SERVE
//! richieste sincrone non deve MAI emettere IPC FS sincrone. userfs gli
//! inoltra relay DEV e resta bloccato finche' non risponde; se nel mentre il
//! driver resta bloccato su userfs (pump read, echo write), nessuno dei due
//! avanza piu' (osservato: wedge t30/t31). Per questo tty e' un client FS
//! PURAMENTE async (`read_async`/`write_async`/`open_async`/
//! `fs_register_async` + collect via poll): non si blocca mai, risponde alle
//! relay all'istante (i DEV_WRITE vengono accodati e recapitati in background:
//! le scritte su console/VGA non possono fallire).
//!
//! EVENT-DRIVEN (anti-dilution): tty dorme in `recv()` bloccante e si sveglia
//! solo su reply async / EXIT_NOTIFY / KBD_NOTIFY (kbd lo avvisa quando ci
//! sono scancode) / relay DEV. Niente pump in polling: un server che gira a
//! vuoto ruba quanta a tutti (osservato: flooder t30 rallentato 25x da UN
//! solo spinner a pari priorita'). L'unico codice sincrono e' pre-
//! registrazione (nessuno puo' instradargli relay: il mount non esiste ancora).

#![no_std]
#![no_main]

extern crate alloc;
use alloc::collections::VecDeque;
use pc_keyboard::{DecodedKey, HandleControl, KeyCode, Keyboard, ScancodeSet1, layouts};

use libr::println;

// ── IPC tags (devono combaciare con userfs) ─────────────────────────

const DEV_OPEN: u64 = 0x20;
const DEV_READ: u64 = 0x21;
const DEV_WRITE: u64 = 0x22;
const DEV_CLOSE: u64 = 0x23;

/// Notify da userkbd (fire-and-forget, NESSUNA reply): scancode in attesa.
/// tty dorme in recv() e si sveglia solo qui (o su relay DEV / reply async).
/// Senza notify servirebbe pump in polling (sempre Ready → dilution scheduler).
const KBD_NOTIFY: u64 = 0x40;

/// Device type per DEV_OPEN (stesso di prima: il path non cambia).
const DEV_KEYBOARD: u64 = 2;

const ERR: u64 = !0u64;

// ── Ring I/O (Fase 10.2, pattern console/devfs: servire i client) ───

const CLI_REQ: u64 = libr::CLI_REQ_VA;
const CLI_RESP: u64 = libr::CLI_RESP_VA;
const RING_DATA_CAP: usize = 4088;
const RING_HEAD: usize = 0xFF8;
const RING_TAIL: usize = 0xFFC;

/// Scrive dati nella response ring del client (a CLI_RESP).
unsafe fn resp_ring_write_client(data: &[u8]) {
    let frame_len = 16 + data.len();
    unsafe {
        let head = core::ptr::read_volatile((CLI_RESP + RING_HEAD as u64) as *const u32);
        let mut hdr = [0u8; 16];
        hdr[0..8].copy_from_slice(&(data.len() as u64).to_le_bytes());
        hdr[8..16].copy_from_slice(&0u64.to_le_bytes());
        let dst = CLI_RESP as *mut u8;
        for (i, byte) in hdr.iter().enumerate() {
            let p = ((head as usize) + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        for (i, byte) in data.iter().enumerate() {
            let p = ((head as usize) + 16 + i) % RING_DATA_CAP;
            core::ptr::write_volatile(dst.add(p), *byte);
        }
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((CLI_RESP + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// Legge `count` byte dalla request ring del client e avanza la tail di
/// (20 + letti): serve a consumare il payload dei DEV_WRITE inoltrati.
unsafe fn req_ring_read_client(dst: &mut [u8], count: usize) -> usize {
    unsafe {
        let tail = core::ptr::read_volatile((CLI_REQ + RING_TAIL as u64) as *const u32);
        let src = CLI_REQ as *const u8;
        let n = count.min(dst.len());
        for i in 0..n {
            let p = ((tail as usize) + 20 + i) % RING_DATA_CAP;
            dst[i] = core::ptr::read_volatile(src.add(p));
        }
        let new_tail = ((tail as usize) + 20 + n) % RING_DATA_CAP;
        core::ptr::write_volatile((CLI_REQ + RING_TAIL as u64) as *mut u32, new_tail as u32);
        n
    }
}

// ── Stato ───────────────────────────────────────────────────────────

const INPUT_CAPACITY: usize = 256;
/// Coda output verso /dev/console: echo + DEV_WRITE inoltrati, in ordine.
/// Le scritte VGA non falliscono mai: le relay DEV_WRITE rispondono OK subito.
const OUT_CAPACITY: usize = 16384;
#[derive(Clone, Copy, PartialEq)]
enum OpKind {
    BufReg,
    OpenKbd,
    OpenCon,
    Register,
    PumpRead,
    ConWrite,
}

#[derive(Clone, Copy)]
struct Pending {
    req: i64,
    kind: OpKind,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    LookupFs,
    BufReg,
    OpenKbd,
    OpenCon,
    Register,
    Steady,
}

struct Tty {
    phase: Phase,
    pending: Option<Pending>,
    kbd_fd: i64,
    con_fd: i64,
    input: VecDeque<u8>,
    out: alloc::vec::Vec<u8>,
    decoder: Keyboard<layouts::Us104Key, ScancodeSet1>,
    err_streak: u32,
    ready_sent: bool,
    flush_wait_until: i64,
    /// Pump richiesta (notify kbd o ingresso in Steady): il prossimo giro
    /// avvia una read async (level-triggered: resta finche' non parte).
    pump_now: bool,
    /// Backoff dopo un pump fallito (vedi retry in collect PumpRead).
    pump_wait_until: i64,
    /// Byte digitati sulla riga corrente (disciplina di linea, Fase 18.0):
    /// l'output della shell (prompt incluso) NON passa da `emit`, quindi il
    /// contatore misura solo l'eco digitato — il backspace puo' cancellare
    /// solo quello che l'utente ha scritto.
    line_len: usize,
}

impl Tty {
    fn new() -> Self {
        Self {
            phase: Phase::LookupFs,
            pending: None,
            kbd_fd: -1,
            con_fd: -1,
            input: VecDeque::new(),
            out: alloc::vec::Vec::new(),
            decoder: Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::MapLettersToUnicode,
            ),
            err_streak: 0,
            ready_sent: false,
            flush_wait_until: 0,
            pump_now: false,
            pump_wait_until: 0,
            line_len: 0,
        }
    }

    /// Torna allo stato pre-boot (morte userfs o streak di errori): scarta
    /// l'op in volo e i fd; il loop riparte dal lookup Fs. Idempotente.
    /// Stampa SEMPRE: un reset inatteso deve essere visibile (lo stallo
    /// silenzioso di Fase 15 e' costato un giorno di diagnosi).
    fn reset_to_lookup(&mut self) {
        println!("[usertty] reset boot SM (phase={:?})", self.phase as u8);        libr::fs_abort_pending();
        self.pending = None;
        self.phase = Phase::LookupFs;
        self.kbd_fd = -1;
        self.con_fd = -1;
        self.input.clear();
        self.out.clear();
        self.line_len = 0;
        self.err_streak = 0;
    }

    fn note_error(&mut self) {
        self.err_streak += 1;
        // Il peer driver (kbd/console) potrebbe essere stato riavviato senza
        // che ce ne accorgessimo (nessun EXIT a noi: non siamo suoi peer):
        // dopo 50 errori consecutivi riparti dal lookup (riapre i peer).
        if self.err_streak >= 50 {
            println!("[usertty] streak errori, riapro i peer");
            self.reset_to_lookup();
        }
    }

    /// Avanza il boot async: una transizione per chiamata, mai bloccante.
    /// INVARIANTE FONDAMENTALE: l'invio avviene NELLA STESSA chiamata che
    /// entra nella fase (mai "imposto fase ora, invio al prossimo giro"):
    /// prima della registrazione nessuno puo' svegliare tty, quindi un giro
    /// che si chiude con recv() bloccante senza aver inviato nulla dorme per
    /// sempre (osservato: stallo silenzioso al boot con recv bloccante).
    fn boot_step(&mut self) {
        // Una sola op in volo (formato frame senza lunghezza): se c'e' gia'
        // una pending, la collect la chiude al giro dopo.
        if self.pending.is_some() {
            return;
        }
        loop {
            match self.phase {
                Phase::LookupFs => {
                    if libr::service_lookup(libr::Service::Fs).is_ok() {
                        self.phase = Phase::BufReg;
                        continue;
                    } else {
                        for _ in 0..100_000 {
                            core::hint::spin_loop();
                        }
                        return;
                    }
                }
            // SEMPRE handshake prima di aprire: i ring di userfs sono
            // indicizzati per canale — sotto un nuovo canale (restart userfs,
            // re-lookup dopo stale) serve un nuovo BUF_REG o ogni op prende
            // NOHANDSHAKE per sempre (osservato Fase 15). Idempotente.
            Phase::BufReg => {
                let req = libr::fs_buf_reg_async();
                if req >= 0 {
                    self.pending = Some(Pending { req, kind: OpKind::BufReg });
                } else {
                    for _ in 0..100_000 {
                        core::hint::spin_loop();
                    }
                }
                return;
            }
            Phase::OpenKbd => {
                // NOTA: il FILE device ("/dev/kbd/kbd"), mai la radice del
                // mount ("/dev/kbd" ha rel="" → dev_type fallisce, EISDIR).
                let req = libr::open_async("/dev/kbd/kbd", 0);
                if req >= 0 {
                    self.pending = Some(Pending { req, kind: OpKind::OpenKbd });
                } else {
                    // Fallimento (backpressure o peer in restart): backoff,
                    // non martellare lookup/send a vuoto (igiene Livello 1).
                    for _ in 0..100_000 {
                        core::hint::spin_loop();
                    }
                }
                return;
            }
            Phase::OpenCon => {
                let req = libr::open_async("/dev/console/console", 0);
                if req >= 0 {
                    self.pending = Some(Pending { req, kind: OpKind::OpenCon });
                } else {
                    for _ in 0..100_000 {
                        core::hint::spin_loop();
                    }
                }
                return;
            }
            Phase::Register => {
                let req = libr::fs_register_async(b"/dev/input");
                if req >= 0 {
                    self.pending = Some(Pending { req, kind: OpKind::Register });
                } else {
                    for _ in 0..100_000 {
                        core::hint::spin_loop();
                    }
                }
                return;
            }
            Phase::Steady => {
                // Peer mai aperti (boot sfortunato): riparti dal lookup.
                if self.kbd_fd < 0 || self.con_fd < 0 {
                    self.reset_to_lookup();
                }
                return;
            }
        }
        }
    }

    /// Raccoglie una reply async che matcha `pending`. Ritorna true se era
    /// nostra (consumata), false altrimenti.
    fn collect_if_mine(&mut self, m: &libr::IpcMsg) -> bool {
        let p = match self.pending {
            Some(p) if m.req_id > 0 && m.req_id == p.req => p,
            _ => return false,
        };
        // Niente remap: i relay usano le finestre dedicate CLI_*, i ring
        // propri non vengono mai rimappati da nessuno.
        let mut tmp = [0u8; 64];
        match p.kind {
            OpKind::BufReg => {
                // Niente frame nel ring per BUF_REG: basta w0==0. MA il guard
                // 1-in-volo va resettato comunque (fs_collect_msg lo fa per le
                // altre op): senza, ogni op successiva viene rifiutata per
                // sempre (osservato: stallo silenzioso al boot).
                libr::fs_abort_pending();
                if m.w0 == 0 {
                    self.err_streak = 0;
                    self.phase = Phase::OpenKbd;
                } else {
                    self.note_error();
                    self.reset_to_lookup();
                    return true;
                }
            }
            OpKind::OpenKbd => {
                let fd = libr::fs_collect_msg(m, &mut tmp, 64, false);
                if fd >= 0 {
                    self.kbd_fd = fd;
                    self.err_streak = 0;
                    self.phase = Phase::OpenCon;
                } else {
                    self.note_error();
                    self.reset_to_lookup();
                    return true;
                }
            }
            OpKind::OpenCon => {
                let fd = libr::fs_collect_msg(m, &mut tmp, 64, false);
                if fd >= 0 {
                    self.con_fd = fd;
                    self.err_streak = 0;
                    self.phase = Phase::Register;
                } else {
                    self.note_error();
                    self.reset_to_lookup();
                    return true;
                }
            }
            OpKind::Register => {
                let r = libr::fs_collect_msg(m, &mut tmp, 64, false);
                if r == 0 {
                    self.err_streak = 0;
                    self.phase = Phase::Steady;
                    println!("[usertty] registered /dev/input with userfs");
                } else {
                    self.note_error();
                    self.reset_to_lookup();
                }
            }
            OpKind::PumpRead => {
                let n = libr::fs_collect_msg(m, &mut tmp, 64, true);
                if n > 0 {
                    self.err_streak = 0;
                    self.decode_bytes(&tmp[..n as usize]);
                } else if n < 0 {
                    // Errore (es. resync userfs che ha scartato il frame):
                    // riprova al prossimo giro invece di aspettare una nuova
                    // notify (che potrebbe non arrivare mai: la notify e' andata
                    // persa col frame scartato e kbd dorme). Il relay DEV_READ
                    // della riprova sveglia kbd da solo: se ha dati li consegna,
                    // se e' vuoto torna 0 e ci si ferma. Solo su ERR, mai su 0
                    // (0 = vuoto legittimo, nessun retry). Throttle via
                    // pump_wait_until (come flush); dopo 50 fallimenti
                    // consecutivi reset_to_lookup riapre i peer.
                    self.note_error();
                    self.pump_now = true;
                    self.pump_wait_until = libr::get_ticks().wrapping_add(2);
                }
                // n == 0 (vuoto): niente da fare, nessun errore.
            }
            OpKind::ConWrite => {
                let r = libr::fs_collect_msg(m, &mut tmp, 64, false);
                if r >= 0 {
                    self.err_streak = 0;
                    let adv = (r as usize).min(self.out.len());
                    self.out.drain(..adv);
                } else {
                    self.note_error();
                }
            }
        }
        self.pending = None;
        true
    }

    /// Decodifica scancode: echo in coda output + push byte cotti in input.
    /// Identico al vecchio comportamento console (char-by-char immediato).
    fn decode_bytes(&mut self, scancodes: &[u8]) {
        for &sc in scancodes {
            if let Ok(Some(event)) = self.decoder.add_byte(sc) {
                if let Some(decoded) = self.decoder.process_keyevent(event) {
                    match decoded {
                        DecodedKey::Unicode(c) => {
                            let mut tmp = [0u8; 4];
                            let s = c.encode_utf8(&mut tmp);
                            self.emit(s.as_bytes());
                        }
                        DecodedKey::RawKey(code) => match code {
                            KeyCode::Return | KeyCode::NumpadEnter => self.emit(b"\n"),
                            KeyCode::Backspace => self.emit(b"\x08"),
                            _ => {}
                        },
                    }
                }
            }
        }
    }

    /// Unico punto che genera sia il byte cotto in input sia l'eco su
    /// console. Conta i digitati sulla riga (`line_len`, reset a `\n`): un
    /// backspace a riga vuota viene ingoiato (niente in input, niente eco) —
    /// la shell fa pop no-op su String vuota, ma l'eco cancellerebbe il
    /// prompt su VGA (la console cancella incondizionatamente).
    fn emit(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                // \n e \x0c (clear, Fase 18.1) riavviano la riga visiva:
                // il contatore riparte da zero in entrambi i casi.
                b'\n' | b'\x0c' => self.line_len = 0,
                0x08 => {
                    if self.line_len == 0 {
                        continue;
                    }
                    self.line_len -= 1;
                }
                _ => self.line_len += 1,
            }
            if self.input.len() < INPUT_CAPACITY {
                self.input.push_back(b);
            }
            if self.out.len() < OUT_CAPACITY {
                self.out.push(b);
            }
        }
    }

    /// Pump tastiera event-driven: parte SOLO se richiesta (notify kbd o
    /// ingresso in Steady), una read async alla volta. Mai polling periodico:
    /// tty dorme in recv() quando idle (zero dilution scheduler).
    fn pump_maybe(&mut self) {
        if self.phase != Phase::Steady || self.pending.is_some() || self.kbd_fd < 0 {
            return;
        }
        if !self.pump_now {
            return;
        }
        // Backoff dopo un invio fallito (come flush): non riprovare a vuoto
        // ogni giro (igiene Livello 1).
        let now = libr::get_ticks();
        if now.wrapping_sub(self.pump_wait_until) < 0 {
            return;
        }
        self.pump_now = false;
        let req = libr::read_async(self.kbd_fd, 64);
        if req >= 0 {
            self.pending = Some(Pending { req, kind: OpKind::PumpRead });
        } else {
            // Invio fallito (backpressure): riprova con backoff, come flush.
            // Senza, un pump perso resta perso fino alla prossima notify.
            self.pump_now = true;
            self.pump_wait_until = libr::get_ticks().wrapping_add(2);
        }
    }

    /// Scarica la coda output verso /dev/console (un chunk async alla volta).
    fn flush_maybe(&mut self) {
        if self.phase != Phase::Steady || self.pending.is_some() {
            return;
        }
        if self.out.is_empty() || self.con_fd < 0 {
            return;
        }
        // Backoff dopo un invio fallito (backpressure/restart): non riprovare
        // a vuoto ogni giro (igiene Livello 1).
        let now = libr::get_ticks();
        if now.wrapping_sub(self.flush_wait_until) < 0 {
            return;
        }
        let n = self.out.len().min(4000);
        let req = libr::write_async(self.con_fd, &self.out[..n]);
        if req >= 0 {
            self.pending = Some(Pending { req, kind: OpKind::ConWrite });
        } else {
            self.flush_wait_until = now.wrapping_add(2);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[usertty] starting, pid={}", libr::getpid());

    // Registra il servizio Tty per nome (supervisione init-restart; i client
    // usano il FS, nessuno risolve questo nome per parlare).
    if libr::service_register(libr::Service::Tty).is_ok() {
        println!("[usertty] registered as service Tty");
    }

    let mut tty = Tty::new();

    loop {
        // 1. Boot async (no-op in Steady).
        //    (Niente remap dance: i relay usano le finestre dedicate CLI_*,
        //    i ring propri non vengono mai rimappati da nessuno.)
        tty.boot_step();
        if tty.phase == Phase::Steady && !tty.ready_sent {
            tty.ready_sent = true;
            // Prima pump: svuota eventuali tasti arrivati prima di noi (kbd li
            // accoda anche senza tty registrato).
            tty.pump_now = true;
            // SVC_READY fire-and-forget (init aspetta a boot): retry bounded.
            for _ in 0..100 {
                if libr::send_async(libr::CHANNEL_PARENT, 0x7D, 1, 0).is_ok() {
                    break;
                }
                for _ in 0..10_000 {
                    core::hint::spin_loop();
                }
            }
        }

        // 2. Pump + flush (solo Steady, mai bloccanti: tutto async).
        tty.pump_maybe();
        tty.flush_maybe();

        // 3. Attesa messaggi. REGOLA FONDAMENTALE (osservato: sleep-forever):
        //    si dorme in recv() bloccante SOLO se c'e' un wake garantito
        //    (pending settata → arrivera' reply o EXIT; Steady → relay DEV /
        //    notify / reply reali, o giusto idle). In boot SENZA pending
        //    (LookupFs, retry dopo send fallita) NESSUNO puo' svegliarci
        //    (mount assente, reply inesistente): li' si POLLA con spin
        //    (come gli ensure loop degli altri driver), mai block.
        let can_sleep = tty.pending.is_some() || tty.phase == Phase::Steady;
        if !can_sleep {
            match libr::recv_poll() {
                Some(m) => tty.handle_msg(m),
                None => {
                    for _ in 0..10_000 {
                        core::hint::spin_loop();
                    }
                    continue;
                }
            };
            continue;
        }
        // 3b. BLOCCANTE: tty dorme qui quando idle o in attesa di reply
        //    (zero dilution scheduler).
        match libr::recv() {
            Ok(m) => tty.handle_msg(m),
            Err(_) => {
                // Peer morto senza EXIT recapitato, o wake spurio: ricontrolla
                // il giro dopo (EXIT_NOTIFY arrivera' e resettera').
            }
        }
    }
}

/// Gestione di un singolo messaggio ricevuto (poll o blocking): reply async
/// (collect/stale), EXIT_NOTIFY (reset), KBD_NOTIFY (pump, mai reply),
/// relay DEV (serve e rispondi subito). Estratta perche' usata sia dal ramo
/// poll (boot senza pending) che da quello bloccante.
impl Tty {
    fn handle_msg(&mut self, m: libr::IpcMsg) {
        if m.req_id > 0 {
            // Risposta async: nostra (collect) o stale (scarta: niente frame
            // nostro nel ring, nessun consumo da fare).
            self.collect_if_mine(&m);
            return;
        }
        if m.tag == libr::EXIT_NOTIFY {
            // userfs morto e rinato (o altro peer): riparte il boot async
            // (riapre i peer, ri-registra). Mai reply.
            println!("[usertty] peer morto, riparto dal lookup");
            self.reset_to_lookup();
            return;
        }
                if m.tag == KBD_NOTIFY {
                    // Fire-and-forget da kbd: NESSUNA reply (il mittente async
                    // non aspetta; rispondergli accoderebbe spazzatura in kbd).
                    self.pump_now = true;
                    return;
                }
        let result: Option<u64> = match m.tag {
            DEV_OPEN => {
                if m.w0 == DEV_KEYBOARD {
                    Some(0)
                } else {
                    None
                }
            }
            DEV_READ => {
                let count = (m.w1 as usize).min(256);
                let mut buf = [0u8; 256];
                let mut i = 0;
                while i < count {
                    match self.input.pop_front() {
                        Some(c) => {
                            buf[i] = c;
                            i += 1;
                        }
                        None => break,
                    }
                }
                // Frame SEMPRE (anche vuoto con i==0, come kbd/devfs): il
                // client distingue "0 byte" da "ring vuoto" solo dal frame.
                // Senza, un async-reader confonde vuoto e risposta persa.
                unsafe { resp_ring_write_client(&buf[..i]); }
                Some(i as u64)
            }
            DEV_WRITE => {
                // Accoda per il flush async e rispondi OK SUBITO: aspettare il
                // completamento qui ricreerebbe il ciclo (userfs aspetta noi,
                // noi userfs).
                let count = m.w1 as usize;
                if count > 0 {
                    let mut data = alloc::vec::Vec::with_capacity(count);
                    data.resize(count, 0);
                    unsafe { req_ring_read_client(&mut data, count); }
                    if self.out.len() + count <= OUT_CAPACITY {
                        self.out.extend_from_slice(&data);
                    }
                }
                Some(count as u64)
            }
            DEV_CLOSE => Some(0),
            _ => None,
        };
        let _ = libr::reply(0, result.unwrap_or(ERR), 0);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    println!("[usertty] panic");
    libr::exit(1)
}
