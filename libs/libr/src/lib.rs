//! libr — libreria di sistema per processi user (equivalente minimale di una
//! libc per rustOS). Fornisce i wrapper alle syscall del kernel.
//!
//! ABI syscall (vedi docs/src/06-syscalls.md — numerazione arbitraria del
//! progetto, NON standard):
//!   - `rax` : numero di syscall
//!   - `rdi` : arg1
//!   - `rsi` : arg2
//!   - `rdx` : arg3
//!   - `r10` : arg4
//!   - ritorno in `rax`
//!
//! Syscall implementate dal kernel (Fase 6):
//!   - 0 = exit(code)
//!   - 2 = write(fd, buf, count)
//!   - 8 = getpid()
//!
//! Nota: `syscall`/`sysret` salvano `RCX` (RIP) e `R11` (RFLAGS) senza toccarli,
//! quindi lo shim li marca come `lateout` per non affermare di conservarli.

#![no_std]

pub extern crate syscall_numbers;
use syscall_numbers::*;

/// Pagina fisica scratch riservata dal kernel per i test `map_physical`.
pub use syscall_numbers::MAP_TEST_PHYS;
/// Servizi di sistema raggiungibili per nome (ADR-0008).
pub use syscall_numbers::Service;
/// Tag della notifica kernel→parent della morte di un figlio (Fase 14).
pub use syscall_numbers::EXIT_NOTIFY;
/// Tag della notify kernel→userkbd su IRQ1 (Fase 15, bridge interrupt→IPC).
pub use syscall_numbers::IRQ_NOTIFY_KBD;
/// Bound di scansione PID per `ps` (Fase 19.1, = MAX_PIDS del kernel).
pub use syscall_numbers::PS_SCAN_MAX;
/// Protocollo DISK_* userfs→userdisk (Fase 16, single source in
/// `syscall-numbers`, Fase 16c): handshake/open/read/close + resolve
/// nome→handle di proprieta' del driver.
pub use syscall_numbers::{DISK_CLOSE, DISK_HELLO, DISK_OPEN, DISK_READ, DISK_RESOLVE};

/// Allocatore globale on-demand (free-list + `sbrk`): unico per tutto il
/// userland. Vive qui cosi' ogni binario che linka `libr` lo usa senza
/// duplicare codice.
pub mod heap;

/// Esegue una syscall a 4 argomenti e ne restituisce il risultato in `rax`.
///
/// # Safety
/// Il numero e gli argomenti devono essere validi per il kernel del target.
#[inline]
pub unsafe fn syscall4(
    number: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => ret,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Come `syscall4`, ma cattura anche i registri di ritorno `rdi/rsi/rdx/r10`:
/// usato dalle syscall IPC (ADR-0008) che restituiscono piu' parole (channel,
/// tag, w0, w1) oltre allo stato in `rax`.
///
/// # Safety
/// Il numero e gli argomenti devono essere validi per il kernel del target.
#[inline(always)]
pub unsafe fn syscall4_out(
    number: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
) -> (i64, u64, u64, u64, u64) {
    let rax: i64;
    let rdi: u64;
    let rsi: u64;
    let rdx: u64;
    let r10: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => rax,
            inlateout("rdi") arg1 => rdi,
            inlateout("rsi") arg2 => rsi,
            inlateout("rdx") arg3 => rdx,
            inlateout("r10") arg4 => r10,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    (rax, rdi, rsi, rdx, r10)
}

/// Messaggio ricevuto da `recv`/`recv_poll` (ADR-0008, Fase 13): porta il
/// canale sorgente (0 = canale di nascita / parent) per le richieste, oppure
/// il request-id (negativo) per una risposta async.
#[derive(Clone, Copy, Debug)]
pub struct IpcMsg {
    /// Per le richieste: il canale sorgente. Per le risposte async resta 0
    /// (il canale e' gia' noto al client) e vale `req_id`.
    pub channel: u64,
    /// Request-id (Fase 13): `> 0` se `recv` ha ricevuto una risposta async a
    /// `-req_id`; `0` se e' una richiesta normale (il kernel non lo espone ai
    /// server che rispondono per reply implicita).
    pub req_id: i64,
    pub tag: u64,
    pub w0: u64,
    pub w1: u64,
}

/// Risposta ricevuta da `send` (ADR-0008).
#[derive(Clone, Copy, Debug)]
pub struct IpcReply {
    pub tag: u64,
    pub w0: u64,
    pub w1: u64,
}

/// `send(channel, tag, w0, w1)`: invia il messaggio sul canale (0 = canale di
/// nascita verso il parent) e resta bloccato finche' il peer non risponde con
/// `reply`. Restituisce la risposta e lo stato (`Ok` se riuscito).
#[inline]
pub fn send(channel: u64, tag: u64, w0: u64, w1: u64) -> Result<IpcReply, ()> {
    let (rax, _rdi, rsi, rdx, r10) =
        unsafe { syscall4_out(SYS_SEND, channel, tag, w0, w1) };
    if rax < 0 {
        return Err(());
    }
    Ok(IpcReply { tag: rsi, w0: rdx, w1: r10 })
}

/// Fase 13 — `send_async(channel, tag, w0, w1)`: come `send` ma NON blocca il
/// mittente: ritorna subito il `req_id` (>= 1) della richiesta, o `Err` se la
/// coda del peer e' piena (backpressure) / canale morto. La risposta del peer
/// va raccolta con `wait_reply(req_id)` o con `recv`/`recv_poll` (un messaggio
/// con `req_id == req_id` atteso).
///
/// Vincolo del primo passo: non mescolare `send` sincrone e richieste async
/// in volo per lo stesso processo; raccogliere le risposte in ordine (FIFO).
#[inline]
pub fn send_async(channel: u64, tag: u64, w0: u64, w1: u64) -> Result<i64, ()> {
    let rax = unsafe { syscall4(SYS_SEND_ASYNC, channel, tag, w0, w1) };
    if rax < 0 {
        return Err(());
    }
    Ok(rax)
}

/// Fase 14 — errore di `wait_reply(req_id)`: perche' la reply attesa non e'
/// arrivata. `ServerDied` porta il pid e l'exit code del processo morto
/// (notifica unificata `EXIT_NOTIFY`): il chiamante sa che il server e' morto
/// e puo' gestirlo (re-lookup, retry, uscita). NOTA: significa "UN peer e'
/// morto", non necessariamente il server atteso — una notifica stale di un
/// server precedente puo' arrivare dopo un re-lookup; confrontare `pid` se
/// serve precisione (retry automatico rimandato: serve init-restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReplyError {
    /// Il processo che avrebbe dovuto rispondere e' morto (pid, exit code).
    ServerDied { pid: u64, code: i64 },
    /// Arrivato un altro messaggio fuori ordine (richiesta inattesa).
    UnexpectedMsg,
    /// Errore di `recv`.
    RecvFailed,
}

/// Fase 13 — `wait_reply(req_id)`: resta bloccato finche' non arriva la
/// risposta async alla richiesta `req_id` (un messaggio ricevuto con
/// `req_id == req_id`), poi la restituisce. Presuppone l'ordine FIFO: se
/// arriva qualcos'altro (fuori ordine o una richiesta) restituisce `Err`.
/// Se arriva una notifica `EXIT_NOTIFY` (Fase 14, notifica unificata) il
/// server e' morto: ritorna `Err(ServerDied)` subito invece di attendere
/// per sempre una reply che non arrivera' mai.
#[inline]
pub fn wait_reply(req_id: i64) -> Result<IpcMsg, WaitReplyError> {
    loop {
        match recv() {
            Ok(m) => {
                if m.req_id == req_id {
                    return Ok(m);
                }
                if is_exit_notify(&m) {
                    return Err(WaitReplyError::ServerDied {
                        pid: m.w1,
                        code: m.w0 as i64,
                    });
                }
                // Fuori ordine / richiesta: non gestito nel primo passo.
                return Err(WaitReplyError::UnexpectedMsg);
            }
            Err(_) => return Err(WaitReplyError::RecvFailed),
        }
    }
}

/// Variante di `wait_reply` che filtra per canale (Fase 14): le notifiche
/// `EXIT_NOTIFY` arrivate su un canale DIVERSO da `chan` sono stale (tardive,
/// di altri peer morti — il parent le riceve tutte) e vengono saltate; solo
/// la notifica sul canale atteso diventa `Err(ServerDied)`. Usato da
/// `fs_collect`, dove il canale FS e' stabile (cachato in `FS_CHAN`) ma il
/// pid del server non e' noto al client.
#[inline]
pub fn wait_reply_chan(req_id: i64, chan: u64) -> Result<IpcMsg, WaitReplyError> {
    loop {
        match recv() {
            Ok(m) => {
                if m.req_id == req_id {
                    return Ok(m);
                }
                if is_exit_notify(&m) {
                    if m.channel != chan {
                        continue; // stale: morte di un altro peer
                    }
                    return Err(WaitReplyError::ServerDied {
                        pid: m.w1,
                        code: m.w0 as i64,
                    });
                }
                return Err(WaitReplyError::UnexpectedMsg);
            }
            Err(_) => return Err(WaitReplyError::RecvFailed),
        }
    }
}

/// `recv()`: resta bloccato finche' non arriva un messaggio, poi lo restituisce.
/// Per una richiesta porta `channel` (canale sorgente); per una risposta async
/// (Fase 13) `req_id` = id della richiesta a cui risponde (e `channel` = 0).
#[inline]
pub fn recv() -> Result<IpcMsg, ()> {
    let (rax, rdi, rsi, rdx, r10) = unsafe { syscall4_out(SYS_RECV, 0, 0, 0, 0) };
    if rax < 0 {
        return Err(());
    }
    Ok(decode_ipc_msg(rdi, rsi, rdx, r10))
}

/// Fase 13 — `recv_poll()`: come `recv` ma se la coda e' vuota ritorna `None`
/// subito (non blocca). Il campo `req_id` di `Some(msg)` e' `> 0` per una
/// risposta async, `0` per una richiesta.
#[inline]
pub fn recv_poll() -> Option<IpcMsg> {
    let (rax, rdi, rsi, rdx, r10) = unsafe { syscall4_out(SYS_RECV_NONBLOCK, 0, 0, 0, 0) };
    if rax < 0 {
        return None;
    }
    Some(decode_ipc_msg(rdi, rsi, rdx, r10))
}

/// Decodifica i registri di ritorno di `recv`/`recv_poll`: se `rdi` (signed) e'
/// negativo e' una risposta async e il request-id della richiesta originale e'
/// `-(rdi as i64)`; altrimenti `rdi` e' il canale sorgente di una richiesta.
#[inline]
fn decode_ipc_msg(rdi: u64, rsi: u64, rdx: u64, r10: u64) -> IpcMsg {
    let signed = rdi as i64;
    if signed < 0 {
        // Risposta async: il kernel espone il req_id negativo in rdi.
        IpcMsg { channel: 0, req_id: -signed, tag: rsi, w0: rdx, w1: r10 }
    } else {
        // Richiesta normale: rdi = canale sorgente.
        IpcMsg { channel: rdi, req_id: 0, tag: rsi, w0: rdx, w1: r10 }
    }
}

/// `reply(tag, w0, w1)`: risponde al mittente del messaggio che stiamo
/// elaborando (ADR-0008).
#[inline]
pub fn reply(tag: u64, w0: u64, w1: u64) -> Result<(), ()> {
    let rax = unsafe { syscall4(SYS_REPLY, tag, w0, w1, 0) };
    if rax < 0 {
        return Err(());
    }
    Ok(())
}

/// Canale di nascita: il figlio lo usa come destinazione per parlare col parent
/// (ADR-0008). `spawn` ritorna il channel id (lato parent) verso il figlio.
pub const CHANNEL_PARENT: u64 = syscall_numbers::CHANNEL_PARENT;

/// `spawn(name)`: chiede al kernel di creare un nuovo processo dal binario
/// embedded chiamato `name`. Il kernel crea il canale di nascita tra il
/// chiamante (parent) e il figlio: il figlio lo usa come canale 0 (parent), il
/// chiamante riceve qui il channel id per parlare col figlio. Ritorna il
/// channel id o `Err` se il nome non e' noto / la creazione fallisce.
#[inline]
pub fn spawn(name: &[u8]) -> Result<i64, ()> {
    let pid = unsafe { syscall4(SYS_SPAWN, name.as_ptr() as u64, name.len() as u64, 0, 0) };
    if pid < 0 {
        return Err(());
    }
    Ok(pid)
}

/// `service_register(service)`: occupa lo slot del servizio (ADR-0008). Il
/// chiamante diventa l'owner raggiungibile per nome. `Err` se gia' occupato.
#[inline]
pub fn service_register(service: Service) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_SERVICE_REGISTER, service as u64, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(()) }
}

/// `service_lookup(service)`: risolve il servizio in un channel verso
/// l'attuale owner. Ritorna il channel id (>= 0) o `Err`.
#[inline]
pub fn service_lookup(service: Service) -> Result<i64, ()> {
    let c = unsafe { syscall4(SYS_SERVICE_LOOKUP, service as u64, 0, 0, 0) };
    if c < 0 { Err(()) } else { Ok(c) }
}

/// Fase 14 (init-restart) — `service_pid(service)`: ritorna il pid
/// dell'attuale owner del servizio, o `Err` se non registrato. Usato per
/// supervisione/diagnostica (es. verificare che un servizio riavviato sia un
/// processo NUOVO, pid diverso dal precedente).
#[inline]
pub fn service_pid(service: Service) -> Result<i64, ()> {
    let p = unsafe { syscall4(SYS_SERVICE_PID, service as u64, 0, 0, 0) };
    if p < 0 { Err(()) } else { Ok(p) }
}

/// `map_physical(phys, virt, count)`: mappa `count` pagine fisiche a partire
/// da `phys` all'indirizzo virtuale `virt` nello spazio del chiamante.
/// Usato dal console server per accedere al frame buffer VGA.
#[inline]
pub fn map_physical(phys: u64, virt: u64, count: usize) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_MAP_PHYSICAL, phys, virt, count as u64, 0) };
    if r < 0 {
        return Err(());
    }
    Ok(())
}

/// Scrive `count` byte da `buf` sul descrittore `fd`. Ritorna i byte scritti,
/// oppure un valore negativo in caso di errore (es. fd non supportato).
#[inline]
pub fn write(fd: u64, buf: *const u8, count: usize) -> i64 {
    unsafe { syscall4(SYS_WRITE, fd, buf as u64, count as u64, 0) }
}

/// Id del processo corrente.
#[inline]
pub fn getpid() -> i64 {
    unsafe { syscall4(SYS_GETPID, 0, 0, 0, 0) }
}

/// Numero di tick PIT trascorsi dall'avvio (100 Hz).
#[inline]
pub fn get_ticks() -> i64 {
    unsafe { syscall4(SYS_GET_TICKS, 0, 0, 0, 0) }
}

/// `sbrk(inc)`: estende l'heap del processo di `inc` byte (arrotondati a
/// pagina dal kernel; nessuna pagina mappata subito, materializzazione lazy
/// al primo accesso). Ritorna il vecchio `heap_brk` (inizio della nuova
/// regione), oppure `Err` se l'estensione non e' possibile.
#[inline]
pub fn sbrk(inc: usize) -> Result<usize, ()> {
    let r = unsafe { syscall4(SYS_SBRK, inc as u64, 0, 0, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(r as usize)
    }
}


/// Termina il processo corrente con il codice `code`. Non ritorna.
#[inline]
pub fn exit(code: i64) -> ! {
    unsafe {
        syscall4(SYS_EXIT, code as u64, 0, 0, 0);
    }
    unsafe {
        core::arch::asm!("ud2", options(noreturn));
    }
}

/// Fase 14 — `kill(pid, code)`: chiede al kernel di terminare il processo
/// user `pid` con il codice `code` (cleanup differito + cascata sulla
/// discendenza + notifica `EXIT_NOTIFY` al parent). `Ok` se il processo e'
/// stato terminato, `Err` se il pid non esiste / non e' killabile (init,
/// processi kernel, se stesso).
#[inline]
pub fn kill(pid: i64, code: i64) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_KILL, pid as u64, code as u64, 0, 0) };
    if r < 0 {
        Err(())
    } else {
        Ok(())
    }
}

/// Fase 14 — `is_exit_notify(m)`: true se `m` e' la notifica kernel→parent
/// della morte di un figlio (`EXIT_NOTIFY`: w0 = exit code, w1 = pid del
/// figlio). I loop `recv` dei server/test devono ignorarla o gestirla.
#[inline]
pub fn is_exit_notify(m: &IpcMsg) -> bool {
    m.tag == EXIT_NOTIFY
}

/// Fase 19.1 — entry `ps`: snapshot di un processo (syscall 37, layout dei
/// campi in `syscall-numbers::SYS_PS_INFO`). `name` = byte del nome (max 16,
/// stop al primo NUL), `parent` = pid del padre (`None` per init/idle).
#[derive(Clone, Copy, Debug)]
pub struct PsEntry {
    pub pid: u32,
    pub name: [u8; 16],
    pub state: u8,
    pub prio: u8,
    pub parent: Option<u32>,
    pub ipc: u8,
    pub ticks: u64,
}

impl PsEntry {
    /// Lunghezza del nome (stop al primo NUL).
    pub fn name_len(&self) -> usize {
        self.name.iter().position(|&b| b == 0).unwrap_or(16)
    }
    /// Nome come `&str` ("?" se non UTF-8, mai in pratica: nomi statici).
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len()]).unwrap_or("?")
    }
}

/// `ps_info(pid)`: snapshot del processo `pid`, `None` se lo slot e' vuoto o
/// il processo e' terminato (come `ps` salta i PID morti).
pub fn ps_info(pid: u32) -> Option<PsEntry> {
    let (rax, rdi, rsi, rdx, r10) =
        unsafe { syscall4_out(SYS_PS_INFO, pid as u64, 0, 0, 0) };
    if rax != 0 {
        return None;
    }
    let mut name = [0u8; 16];
    name[..8].copy_from_slice(&rdi.to_le_bytes());
    name[8..].copy_from_slice(&rsi.to_le_bytes());
    let parent_raw = ((rdx >> 16) & 0xFF) as u32;
    Some(PsEntry {
        pid,
        name,
        state: (rdx & 0xFF) as u8,
        prio: ((rdx >> 8) & 0xFF) as u8,
        parent: if parent_raw == 0 { None } else { Some(parent_raw - 1) },
        ipc: ((rdx >> 24) & 0xFF) as u8,
        ticks: r10,
    })
}

/// Scrive una stringa su stdout (fd 1) bypassando il line buffer.
/// Usare solo per dati binari/raw; per output di testo usare `print!`/`println!`.
#[inline]
pub fn write_stdout(buf: *const u8, count: usize) -> i64 {
    write(1, buf, count)
}

/// Scrive byte su stdout (fd 1) bypassando il line buffer.
/// Compat: usato dai programmi userspace esistenti.
#[inline]
pub fn print_string(s: &[u8]) -> i64 {
    write(1, s.as_ptr(), s.len())
}

// ── Line buffer (flush-on-newline) ────────────────────────────────

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

const LINE_BUF_SIZE: usize = 1024;
static mut LINE_BUF: [u8; LINE_BUF_SIZE] = [0u8; LINE_BUF_SIZE];
static LINE_LEN: AtomicUsize = AtomicUsize::new(0);

/// Svuota il line buffer scrivendo il contenuto su stdout (fd 1) tramite
/// una singola syscall, poi resetta il buffer.
pub fn flush() {
    let len = LINE_LEN.swap(0, Ordering::Relaxed);
    if len > 0 {
        let ptr = core::ptr::addr_of!(LINE_BUF) as *const u8;
        write(1, ptr, len);
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

// ── FS wrappers (Fase 10.2): ring buffer SPSC + IPC diretta a userfs ──
//
// Ogni processo ha DUE pagine ring (request + response) allocate dalla
// syscall 26 (`SYS_RING_ALLOC`) e mappate a `REQ_RING_VA` e `RESP_RING_VA`.
// Le pagine vengono registrate presso userfs con una IPC `FS_BUF_REG`.
// Le operazioni FS scrivono un request frame nel request ring, notificano
// userfs con `FS_NOTIFY`, e leggono il response frame dalla response ring
// dopo la reply IPC.

// ── Ring buffer constants ─────────────────────────────────────────

/// Request ring virtuale (coincide con USER_FS_BUFFER del kernel).
const REQ_RING_VA: u64 = 0x0000_4000_0020_0000;
/// Response ring virtuale (USER_FS_BUFFER + 0x1000).
const RESP_RING_VA: u64 = 0x0000_4000_0021_0000;
/// Finestre DEDICATE per i relay userfs→driver (zero-copy senza clobber):
/// userfs inietta qui (via `map_in`) i ring del client quando inoltra una DEV_*.
/// Separate dalle finestre proprie (REQ/RESP): i ring propri di un driver-server
/// non vengono mai rimappati da nessuno, quindi niente `remap` dance, niente
/// race di preemption tra remap e uso (osservato: letture congelate/wedge).
/// Libere nella mappa user (heap da +0x400000, stack sotto, VGA +0x100000).
pub const CLI_REQ_VA: u64 = 0x0000_4000_0022_0000;
/// Finestra response per i relay userfs→driver (vedi sopra).
pub const CLI_RESP_VA: u64 = 0x0000_4000_0023_0000;
/// Capacita' dati per ring (4088 byte; gli ultimi 8 byte della pagina
/// 4KiB = head + tail a 0xFF8/0xFFC, fuori dall'area dati).
const RING_DATA_CAP: usize = 4088;
/// Dimensione massima di un payload dati in un singolo frame del ring.
/// Il response frame occupa 16 B di header: il payload utile massimo e'
/// RING_DATA_CAP - 16. Un valore sotto il massimo lascia margine per il
/// wrap e per eventuali frame non ancora letti.
const RING_MAX_PAYLOAD: usize = 4000;
/// Offset head nel ring page.
const RING_HEAD: usize = 0xFF8;
/// Offset tail nel ring page.
const RING_TAIL: usize = 0xFFC;

// ── Tag delle operazioni (nel frame del ring, non nell'IPC) ────────
// Single source in `syscall-numbers` (Fase 17): prima duplicati qui, in
// userfs e (R_REGISTER) userdisk.
pub use syscall_numbers::{
    R_CLOSE, R_DELETE, R_MKDIR, R_MOUNT, R_OPEN, R_READ, R_READDIR, R_REGISTER, R_UMOUNT,
    R_WRITE, R_RIGHTS_DROP, R_RIGHTS_GET,
};
/// Bit dei diritti per-canale (Fase 17, self-restriction; DELETE in 18.2):
/// mask per `rights_drop`, valore di ritorno di `rights_get`.
pub use syscall_numbers::{
    RIGHTS_ALL, RIGHTS_DELETE, RIGHTS_MKDIR, RIGHTS_MOUNT, RIGHTS_OPEN, RIGHTS_READ,
    RIGHTS_READDIR, RIGHTS_UMOUNT, RIGHTS_WRITE,
};

/// Flag `open` (Fase 18.2): crea il file se non esiste.
pub use syscall_numbers::O_CREAT;

/// IPC tag: il client ha scritto nel request ring e notifica il server.
const FS_NOTIFY: u64 = 0x32;
/// IPC tag: un driver registra il proprio prefix di mount (frame R_REGISTER
/// nel request ring). Deve combaciare con la costante FS_REGISTER di userfs.
const FS_REGISTER: u64 = 0x30;
/// Handshake: "i miei ring buffer hanno fisico req=w0, resp=w1".
const FS_BUF_REG: u64 = 0x31;

const ERR: u64 = !0u64;
/// Deve combaciare con `ERR_NOHANDSHAKE` di userfs: il server non conosce i
/// nostri ring (riavviato dopo la registrazione) → rifare handshake + 1 redo.
const ERR_NOHANDSHAKE: u64 = !0u64 - 1;

static FS_INITED: AtomicBool = AtomicBool::new(false);

/// Guard "1 operazione FS async in volo per processo" (Fase 13): quando e' -1
/// nessuna op async e' in volo; altrimenti contiene il req_id dell'op da
/// raccogliere. I wrapper FS sincroni e il prossimo async si rifiutano
/// (-1) finche' non si raccoglie, per non mischiare frame nel ring (il formato
/// frame non ha lunghezza payload esplicita: un solo frame per volta).
static FS_PENDING: AtomicI64 = AtomicI64::new(-1);

/// Canale verso il fs server (ADR-0008): risolto per nome (`Fs`) alla prima
/// operazione e cachato. `-1` = non ancora risolto.
static FS_CHAN: AtomicI64 = AtomicI64::new(-1);

/// Fisici dei ring per-processo (Fase 14, t28): salvati al primo handshake per
/// poterlo RIPETERE dopo un restart di userfs (le pagine persistono nel
/// processo, ma il nuovo server non conosce la registrazione).
static REQ_PHYS: AtomicU64 = AtomicU64::new(0);
static RESP_PHYS: AtomicU64 = AtomicU64::new(0);

/// Risolve (una volta) il canale verso il fs server per nome.
fn fs_chan() -> i64 {
    let c = FS_CHAN.load(Ordering::Relaxed);
    if c >= 0 {
        return c;
    }
    // Race di boot: userfs potrebbe non essersi ancora registrato come Fs. Con
    // la vecchia send al PID 4 il mittente restava bloccato finche' userfs era
    // pronto; col lookup per nome il servizio potrebbe non esistere ancora.
    // Replica il comportamento bloccante: ritenta finche' Fs non si registra,
    // con lunghi spin puri tra i lookup (IF=1) per non affamare il timer e
    // lasciare a userfs il tempo di partire. A boot userfs e' garantito.
    loop {
        if let Ok(chan) = service_lookup(Service::Fs) {
            FS_CHAN.store(chan, Ordering::Relaxed);
            return chan;
        }
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
    }
}

// ── Ring I/O helpers ──────────────────────────────────────────────

/// Legge head e tail dal ring a `ring_va`.
unsafe fn ring_positions(ring_va: u64) -> (u32, u32) {
    let head = unsafe { core::ptr::read_volatile((ring_va + RING_HEAD as u64) as *const u32) };
    let tail = unsafe { core::ptr::read_volatile((ring_va + RING_TAIL as u64) as *const u32) };
    (head, tail)
}

/// Quanti byte di dati sono disponibili nel ring (producer=head, consumer=tail).
/// Head e tail sono wrapped in [0, RING_DATA_CAP).
fn ring_available(head: u32, tail: u32) -> usize {
    ((head + RING_DATA_CAP as u32 - tail) % RING_DATA_CAP as u32) as usize
}

/// Quanti byte di spazio libero ci sono nel ring (max usabile = CAP - 1).
fn ring_free_space(head: u32, tail: u32) -> usize {
    RING_DATA_CAP - 1 - ring_available(head, tail)
}

/// Scrive `data` nel ring a `ring_va` partendo dalla posizione `head`.
/// Avanza head di `data.len()`. Non verifica lo spazio (chiamante deve farlo).
unsafe fn ring_write_at(ring_va: u64, head: u32, data: &[u8]) {
    let dst = ring_va as *mut u8;
    for (i, byte) in data.iter().enumerate() {
        let pos = ((head as usize) + i) % RING_DATA_CAP;
        unsafe { core::ptr::write_volatile(dst.add(pos), *byte); }
    }
}

/// Legge `count` byte dal ring a `ring_va` partendo dalla posizione `tail`.
unsafe fn ring_read_at(ring_va: u64, tail: u32, dst: &mut [u8], count: usize) {
    let src = ring_va as *const u8;
    for i in 0..count.min(dst.len()) {
        let pos = ((tail as usize) + i) % RING_DATA_CAP;
        dst[i] = unsafe { core::ptr::read_volatile(src.add(pos)) };
    }
}

/// Scrive un frame nel request ring. Formato: [tag:4][w0:8][w1:8][payload].
/// Ritorna true se il frame e' stato scritto, false se non c'e' spazio.
fn req_ring_write(tag: u32, w0: u64, w1: u64, payload: &[u8]) -> bool {
    let frame_len = 20 + payload.len();
    unsafe {
        let (head, tail) = ring_positions(REQ_RING_VA);
        if ring_free_space(head, tail) < frame_len {
            return false;
        }
        // Scrivi header (tag + w0 + w1)
        let mut hdr = [0u8; 20];
        hdr[0..4].copy_from_slice(&tag.to_le_bytes());
        hdr[4..12].copy_from_slice(&w0.to_le_bytes());
        hdr[12..20].copy_from_slice(&w1.to_le_bytes());
        ring_write_at(REQ_RING_VA, head, &hdr);
        // Scrivi payload (ring_write_at gestisce il wrap con % RING_DATA_CAP)
        if !payload.is_empty() {
            ring_write_at(REQ_RING_VA, (head + 20) % RING_DATA_CAP as u32, payload);
        }
        // Aggiorna head (wrappa entro [0, RING_DATA_CAP))
        let new_head = ((head as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((REQ_RING_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
    true
}

/// Legge un frame dal response ring. Formato: [result:8][w1:8][payload].
/// Ritorna (result, w1, payload_len) o None se il ring e' vuoto.
fn resp_ring_read() -> Option<(u64, u64, usize)> {
    unsafe {
        let (head, tail) = ring_positions(RESP_RING_VA);
        if ring_available(head, tail) < 8 {
            return None;
        }
        // Leggi result (8 byte)
        let mut result_bytes = [0u8; 8];
        ring_read_at(RESP_RING_VA, tail, &mut result_bytes, 8);
        let result = u64::from_le_bytes(result_bytes);
        // Leggi w1 (8 byte)
        let mut w1_bytes = [0u8; 8];
        ring_read_at(RESP_RING_VA, tail + 8, &mut w1_bytes, 8);
        let w1 = u64::from_le_bytes(w1_bytes);
        // Calcola lunghezza payload
        let total = ring_available(head, tail);
        let payload_len = if total > 16 { total - 16 } else { 0 };
        Some((result, w1, payload_len))
    }
}

/// Legge i payload bytes dal response ring (dopo result+w1).
fn resp_ring_read_payload(dst: &mut [u8], payload_len: usize) {
    unsafe {
        let (_, tail) = ring_positions(RESP_RING_VA);
        ring_read_at(RESP_RING_VA, (tail + 16) % RING_DATA_CAP as u32, dst, payload_len);
        // Avanza tail (wrappa entro [0, RING_DATA_CAP))
        let total = 16 + payload_len;
        let new_tail = ((tail as usize) + total) % RING_DATA_CAP;
        core::ptr::write_volatile((RESP_RING_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
    }
}

/// Avanza la tail del response ring (per frame letti senza payload).
fn resp_ring_consume(frame_len: usize) {
    unsafe {
        let (_, tail) = ring_positions(RESP_RING_VA);
        let new_tail = ((tail as usize) + frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((RESP_RING_VA + RING_TAIL as u64) as *mut u32, new_tail as u32);
    }
}

/// Riporta indietro la head del request ring di `frame_len` byte: usato per
/// "disfare" un frame appena scritto quando la successiva `send_async` fallisce
/// (coda piena / canale morto). Sicuro perche' il frame non e' mai stato
/// notificato: il server non puo' averlo consumato (tail ferma).
fn req_ring_rollback(frame_len: usize) {
    unsafe {
        let (head, _tail) = ring_positions(REQ_RING_VA);
        let new_head = (head as usize + RING_DATA_CAP - frame_len) % RING_DATA_CAP;
        core::ptr::write_volatile((REQ_RING_VA + RING_HEAD as u64) as *mut u32, new_head as u32);
    }
}

/// C'e' un'operazione FS async in volo (guad 1-in-volo, Fase 13)?
fn fs_async_pending() -> bool {
    FS_PENDING.load(Ordering::Relaxed) != -1
}

/// Converte il result di una reply FS in i64: `!0` (ERR) → -1.
fn fs_reply_val(w0: u64) -> i64 {
    if w0 == ERR { -1 } else { w0 as i64 }
}

/// Bound per il re-lookup runtime (Fase 14, init-restart): ~200 tick di spin
/// totali tra i tentativi. Il boot path (`fs_chan`) resta unbounded (garanzia
/// di boot); a runtime un restart rotto deve dare -1 rumoroso, non hang.
const FS_RELOOKUP_TICKS: i64 = 200;

/// Periodo canonico di polling (Livello 1, buon vicinato): ~20 tick tra i
/// tentativi di operativita'. Ogni tentativo e' un round-trip servito da
/// userfs: martellarlo in busy-loop affama gli altri client (osservato
/// t27/t28: mount di devfs ritardato da 10 s+ a ms col throttling).
pub const POLL_PERIOD_TICKS: i64 = 20;

/// Attesa di operativita' con throttling (Livello 1, buon vicinato): chiama
/// `f()` ogni `period_ticks` finche' ritorna true o scade `bound_ticks`
/// (clock `get_ticks`). Ritorna true se `f()` ha avuto successo.
/// MAI busy-loop su syscall FS/IPC nei chiamanti: usare questa.
pub fn poll_wait(bound_ticks: i64, period_ticks: i64, mut f: impl FnMut() -> bool) -> bool {
    poll_value(bound_ticks, period_ticks, || if f() { Some(()) } else { None }).is_some()
}

/// Variante di `poll_wait` che ritorna il valore prodotto da `f()`
/// (`None` = "non ancora pronto, riprova"). Utile quando serve il risultato
/// del tentativo riuscito (fd, pid, ...), non solo un booleano.
pub fn poll_value<T>(bound_ticks: i64, period_ticks: i64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let t0 = get_ticks();
    let mut last = t0.wrapping_sub(period_ticks);
    loop {
        let now = get_ticks();
        if now.wrapping_sub(last) >= period_ticks {
            last = now;
            if let Some(v) = f() {
                return Some(v);
            }
        }
        if get_ticks() - t0 > bound_ticks {
            return None;
        }
        for _ in 0..512 {
            core::hint::spin_loop();
        }
    }
}

/// Apre `path` riprovando throttled fino a `bound_ticks` (vedi `poll_wait`).
/// Ritorna l'fd o -1 a timeout. Sostituisce i busy-loop di open nei test e
/// negli helper: un device non ancora registrato non giustifica mai una
/// tempesta di open verso userfs.
pub fn open_wait(path: &str, flags: u32, bound_ticks: i64, period_ticks: i64) -> i64 {
    poll_value(bound_ticks, period_ticks, || {
        let fd = open(path, flags);
        if fd >= 0 { Some(fd) } else { None }
    })
    .unwrap_or(-1)
}

/// Risolve il canale verso il fs server con attesa BOUNDED (init-restart):
/// ritenta finche' il servizio si registra o scade il bound. Aggiorna la
/// cache e ritorna il channel, o -1.
fn fs_chan_rt() -> i64 {
    let t0 = get_ticks();
    loop {
        if let Ok(chan) = service_lookup(Service::Fs) {
            FS_CHAN.store(chan, Ordering::Relaxed);
            return chan;
        }
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
        if get_ticks() - t0 > FS_RELOOKUP_TICKS {
            return -1;
        }
    }
}

/// Invia al fs server sul canale risolto per nome (ADR-0008). Se la send
/// fallisce (server morto, canale invalidato), invalida la cache, ri-risolve
/// (bounded: attende un eventuale restart da init) e ritenta UNA volta sola;
/// poi -1, mai loop infiniti.
/// Caveat write (at-least-once): se il server applica e poi muore prima della
/// reply, il retry duplica. Per ramfs/devfs-console l'effetto e' benigno
/// (overwrite degli stessi byte / device idempotenti); policy fine futura.
fn fs_send(tag: u64, w0: u64, w1: u64) -> Result<IpcReply, ()> {
    let c = fs_chan();
    if c >= 0 {
        if let Ok(r) = send(c as u64, tag, w0, w1) {
            return Ok(r);
        }
        // Canale morto: invalida e ritenta una volta sola.
        FS_CHAN.store(-1, Ordering::Relaxed);
    }
    let c2 = fs_chan_rt();
    if c2 < 0 {
        return Err(());
    }
    send(c2 as u64, tag, w0, w1)
}

/// Inizializza (una sola volta) i ring buffer del processo: li alloca col
/// kernel e li registra presso userfs con l'handshake `FS_BUF_REG`.
/// Ritorna false se userfs non e' ancora pronto (il chiamante ritentera').
fn fs_init() -> bool {
    if FS_INITED.load(Ordering::Relaxed) {
        return true;
    }
    // syscall SYS_RING_ALLOC: ritorna (req_phys, rdi=resp_phys)
    let (rax, rdi, _rsi, _rdx, _r10) = unsafe {
        syscall4_out(SYS_RING_ALLOC, 0, 0, 0, 0)
    };
    if rax < 0 {
        return false;
    }
    let req_phys = rax as u64;
    let resp_phys = rdi;
    // Registra entrambi gli indirizzi fisici presso userfs
    match fs_send(FS_BUF_REG, req_phys, resp_phys) {
        Ok(_) => {
            REQ_PHYS.store(req_phys, Ordering::Relaxed);
            RESP_PHYS.store(resp_phys, Ordering::Relaxed);
            FS_INITED.store(true, Ordering::Relaxed);
            true
        }
        Err(()) => false,
    }
}

/// Ripete l'handshake ring dopo un restart di userfs (Fase 14, t28): le pagine
/// persistono nel processo, si re-invia solo la coppia phys salvata. AZZERA
/// anche entrambi i ring (head=tail=0): qualunque contenuto appartiene
/// all'epoca morta (frame scritti ma mai notificati/consumati, risposte
/// orfane di reply perse) e disallineerebbe permanentemente client e server.
/// Il chiamante DEVE riscrivere il frame corrente dopo (vedi `redo` in
/// `fs_notify_result`). Ritorna false se il server non c'e'.
fn fs_rehandshake() -> bool {
    let req_phys = REQ_PHYS.load(Ordering::Relaxed);
    let resp_phys = RESP_PHYS.load(Ordering::Relaxed);
    if req_phys == 0 {
        return false;
    }
    match fs_send(FS_BUF_REG, req_phys, resp_phys) {
        Ok(_) => {
            FS_INITED.store(true, Ordering::Relaxed);
            unsafe {
                ring_reset(REQ_RING_VA);
                ring_reset(RESP_RING_VA);
            }
            true
        }
        Err(()) => false,
    }
}

/// Rimappa i PROPRI ring alle finestre fisse (Fase 14, t28): userfs inietta i
/// ring dei client nei driver via `map_in` sulle STESSE VA condivise,
/// sovrascrivendo il mapping dei ring propri del driver senza ripristinarlo.
/// Prima di usare i propri ring (es. `ensure_mounted`), il driver deve
/// richiamare questa (le pagine persistono, basta rimappare). Ritorna false
/// se i ring non sono mai stati allocati.
pub fn fs_remap_self() -> bool {
    let req_phys = REQ_PHYS.load(Ordering::Relaxed);
    let resp_phys = RESP_PHYS.load(Ordering::Relaxed);
    if req_phys == 0 || resp_phys == 0 {
        return false;
    }
    if map_physical(req_phys, REQ_RING_VA, 1).is_err() {
        return false;
    }
    map_physical(resp_phys, RESP_RING_VA, 1).is_ok()
}

/// Alloca una coppia di pagine ring (request, response) SENZA handshake
/// (Fase 16, data-plane `DISK_*` di userdisk): il server riporta i fisici al
/// client nel frame di `DISK_HELLO`, il client li mappa nelle proprie finestre
/// con `map_physical`. Separata dalle pagine FS proprie: niente interleaving
/// di protocolli diversi nello stesso ring (lezione CLI_* del fix kbd/tty).
/// Ritorna `(req_phys, resp_phys)` o `None`.
pub fn ring_alloc_raw() -> Option<(u64, u64)> {
    let (rax, rdi, _rsi, _rdx, _r10) = unsafe {
        syscall4_out(SYS_RING_ALLOC, 0, 0, 0, 0)
    };
    if rax < 0 {
        return None;
    }
    Some((rax as u64, rdi))
}

/// Azzera un ring SPSC (head=tail=0): tutto il contenuto pendente appartiene
/// a un'epoca morta (server riavviato). Solo per `fs_rehandshake`.
unsafe fn ring_reset(ring_va: u64) {
    unsafe {
        core::ptr::write_volatile((ring_va + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((ring_va + RING_TAIL as u64) as *mut u32, 0);
    }
}

/// Notifica un'operazione (`tag` con w0=w1=0; il frame e' gia' scritto nel
/// request ring) e raccoglie il result dal response ring. Con UN redo su
/// `ERR_NOHANDSHAKE`: rifa l'handshake (che AZZERA i ring, scartando gli
/// stale dell'epoca morta) e riscrive il frame corrente via `rewrite`
/// (altrimenti la rinotifica leggerebbe spazzatura/vuoto). Poi UNA rinotifica;
/// al secondo NOHANDSHAKE, -1. Mai loop infiniti, mai doppi frame.
fn fs_notify_result(tag: u64, rewrite: impl Fn() -> bool) -> Option<(u64, u64, usize)> {
    let mut retried = false;
    loop {
        match fs_send(tag, 0, 0) {
            Ok(rep) => {
                if rep.w0 == ERR_NOHANDSHAKE && !retried && fs_rehandshake() && rewrite() {
                    retried = true;
                    continue;
                }
                return resp_ring_read();
            }
            Err(()) => return None,
        }
    }
}

// ── FS wrappers ───────────────────────────────────────────────────

/// `open(path, flags)`: apre un file tramite il fs server.
/// Ritorna il fd (>=0) o -1 su errore.
#[inline]
pub fn open(path: &str, flags: u32) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_OPEN, path.len() as u64, flags as u64, path.as_bytes()) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || {
        req_ring_write(R_OPEN, path.len() as u64, flags as u64, path.as_bytes())
    }) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `read_fs(fd, dst, max_count)`: legge fino a `max_count` byte dal file.
/// I dati viaggiano nel response ring; se `max_count` supera la capacita' di
/// un singolo frame, la lettura viene spezzata in piu' round trip (Fase 10.2).
/// Ritorna i byte letti (0 = EOF) o -1 su errore.
pub fn read_fs(fd: i64, dst: &mut [u8], max_count: usize) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    let mut got = 0usize;
    while got < max_count {
        let want = (max_count - got).min(RING_MAX_PAYLOAD);
        if !req_ring_write(R_READ, fd as u64, want as u64, &[]) {
            return if got > 0 { got as i64 } else { -1 };
        }
        let n = match fs_notify_result(FS_NOTIFY, || {
            req_ring_write(R_READ, fd as u64, want as u64, &[])
        }) {
            Some((result, _, payload_len)) => {
                if result == ERR {
                    resp_ring_consume(16);
                    if got > 0 {
                        return got as i64;
                    }
                    return -1;
                }
                let avail = (result as usize).min(payload_len).min(max_count - got);
                if avail > 0 {
                    resp_ring_read_payload(&mut dst[got..got + avail], avail);
                } else {
                    resp_ring_consume(16);
                }
                let _ = result;
                avail
            }
            None => {
                if got > 0 {
                    return got as i64;
                }
                return -1;
            }
        };
        if n == 0 {
            break; // EOF
        }
        got += n;
        if n < want {
            break; // read corto (EOF o file piu' corto)
        }
    }
    got as i64
}

/// `write_fs(fd, src, count)`: scrive `count` byte sul file (dal request ring).
/// I dati viaggiano nel request ring; se `count` supera la capacita' di un
/// singolo frame, la scrittura viene spezzata in piu' round trip (Fase 10.2).
/// Ritorna i byte scritti o -1.
pub fn write_fs(fd: i64, src: &[u8], count: usize) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    let mut done = 0usize;
    while done < count {
        let want = (count - done).min(RING_MAX_PAYLOAD);
        if !req_ring_write(R_WRITE, fd as u64, want as u64, &src[done..done + want]) {
            return if done > 0 { done as i64 } else { -1 };
        }
        let n = match fs_notify_result(FS_NOTIFY, || {
            req_ring_write(R_WRITE, fd as u64, want as u64, &src[done..done + want])
        }) {
            Some((result, _, _)) => {
                resp_ring_consume(16);
                let r = fs_reply_val(result);
                if r < 0 {
                    if done > 0 {
                        return done as i64;
                    }
                    return -1;
                }
                (r as usize).min(want)
            }
            None => {
                if done > 0 {
                    return done as i64;
                }
                return -1;
            }
        };
        if n == 0 {
            break;
        }
        done += n;
        if n < want {
            break;
        }
    }
    done as i64
}

/// `close(fd)`: chiude un file descriptor.
#[inline]
pub fn close(fd: i64) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_CLOSE, fd as u64, 0, &[]) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || req_ring_write(R_CLOSE, fd as u64, 0, &[])) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `readdir(path, entries_buf, buf_len)`: legge le entry di una directory.
/// Le entry vengono scritte dal server nel response ring nel formato
/// "name\0name\0...\0\0"; le copiamo in `entries_buf`. Ritorna il numero di
/// entry o -1.
#[inline]
pub fn readdir(path: &str, entries_buf: &mut [u8], buf_len: usize) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_READDIR, path.len() as u64, 0, path.as_bytes()) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || {
        req_ring_write(R_READDIR, path.len() as u64, 0, path.as_bytes())
    }) {
        Some((result, _, payload_len)) => {
            let count = fs_reply_val(result);
            if count >= 0 && payload_len > 0 {
                resp_ring_read_payload(entries_buf, payload_len.min(buf_len));
            } else {
                resp_ring_consume(16);
            }
            count
        }
        None => -1,
    }
}

/// `mkdir(path)`: crea una directory tramite il fs server.
/// Ritorna 0 su successo o -1 su errore.
#[inline]
pub fn mkdir(path: &str) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_MKDIR, path.len() as u64, 0, path.as_bytes()) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || {
        req_ring_write(R_MKDIR, path.len() as u64, 0, path.as_bytes())
    }) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `remove(path)`: cancella un file o una directory VUOTA (Fase 18.2).
/// Solo ramfs: FAT read-only e device remoti rifiutano. Ritorna 0 o -1.
#[inline]
pub fn remove(path: &str) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_DELETE, path.len() as u64, 0, path.as_bytes()) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || {
        req_ring_write(R_DELETE, path.len() as u64, 0, path.as_bytes())
    }) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// Scrive un frame "source\0target\0" e lo notifica (helper di `mount`).
fn mount_frame(source: &str, target: &str) -> bool {
    // Path lunghi al massimo MAX_PATH (256) l'uno + 2 NUL.
    let total = source.len() + 1 + target.len() + 1;
    if total > RING_MAX_PAYLOAD || total > 514 {
        return false;
    }
    let mut buf = [0u8; 520];
    buf[..source.len()].copy_from_slice(source.as_bytes());
    buf[source.len()] = 0;
    buf[source.len() + 1..source.len() + 1 + target.len()].copy_from_slice(target.as_bytes());
    buf[source.len() + 1 + target.len()] = 0;
    req_ring_write(R_MOUNT, total as u64, 0, &buf[..total])
}

/// `mount(source, target)`: monta una sorgente a blocchi (es. "/dev/sda")
/// su un target (es. "/mnt", Fase 16b). Ritorna 0 su successo o -1 su errore
/// (sorgente non disco, target invalido, mount fallito).
#[inline]
pub fn mount(source: &str, target: &str) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !mount_frame(source, target) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || mount_frame(source, target)) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `umount(target)`: smonta un target (Fase 16b). Rifiutato se ci sono fd
/// aperti sotto il target. Ritorna 0 su successo o -1 su errore.
#[inline]
pub fn umount(target: &str) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_UMOUNT, target.len() as u64, 0, target.as_bytes()) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || {
        req_ring_write(R_UMOUNT, target.len() as u64, 0, target.as_bytes())
    }) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `rights_drop(keep_mask, subtree)`: riduce i propri diritti sul canale
/// verso userfs (Fase 17, self-restriction only). Solo shrink: il server fa
/// AND con la mask corrente; il subtree puo' solo restringersi (widen =
/// -1, nessun cambio). `subtree=None` = solo-ops. Ritorna 0 o -1.
/// Irrevocabile per disegno (nessun GRANT: i canali non sono trasferibili).
#[inline]
pub fn rights_drop(keep_mask: u32, subtree: Option<&str>) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    let sub_bytes: &[u8] = match subtree {
        Some(s) => s.as_bytes(),
        None => &[],
    };
    if sub_bytes.len() > 256 {
        return -1;
    }
    if !req_ring_write(
        R_RIGHTS_DROP,
        keep_mask as u64,
        sub_bytes.len() as u64,
        sub_bytes,
    ) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || {
        req_ring_write(
            R_RIGHTS_DROP,
            keep_mask as u64,
            sub_bytes.len() as u64,
            sub_bytes,
        )
    }) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `rights_get(buf)`: legge i propri diritti (Fase 17). Scrive il subtree
/// normalizzato + NUL in `buf` (root = solo NUL) e ritorna la mask ops
/// (0..=RIGHTS_ALL) o -1 su errore. Dimensionare `buf` ≥ 257.
#[inline]
pub fn rights_get(buf: &mut [u8]) -> i64 {
    if buf.is_empty() {
        return -1;
    }
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if !req_ring_write(R_RIGHTS_GET, 0, 0, &[]) {
        return -1;
    }
    match fs_notify_result(FS_NOTIFY, || req_ring_write(R_RIGHTS_GET, 0, 0, &[])) {
        Some((result, w1, payload_len)) => {
            let ops = fs_reply_val(result);
            // Leggi tutto il payload in uno stack buffer (subtree ≤ 256 dal
            // server): un solo consumo 16+len, mai disallineamenti.
            let mut tmp = [0u8; 256];
            let take = payload_len.min(256);
            if take > 0 {
                resp_ring_read_payload(&mut tmp, take);
            } else {
                resp_ring_consume(16);
            }
            if ops < 0 {
                return -1;
            }
            let n = (w1 as usize).min(take).min(buf.len() - 1);
            buf[..n].copy_from_slice(&tmp[..n]);
            buf[n] = 0;
            ops
        }
        None => -1,
    }
}

/// Identità stabile di un volume FAT32 dal boot sector (Fase 16d):
/// `(seriale, label_raw_11B)`. Seriale solo con firma estesa `0x29` (layout
/// standard firma-a-66/volid-67-70, o variante mkfat firma-a-67/volid-68-71);
/// label sempre (11 byte raw, trim a carico del chiamante). `None` se il
/// settore non e' un BPB FAT valido (stessi check minimi di mount: 55AA,
/// bps 512, spc potenza di 2 non zero, almeno una FAT non vuota, root ≥ 2).
/// Usato sia dal parser (`userfs/fat32.rs`) che dallo sniff per-nodo del
/// driver (`userdisk`): un nodo annuncia UUID/label sse monta davvero.
pub fn fat_bpb_identity(boot: &[u8; 512]) -> Option<(Option<u32>, [u8; 11])> {
    if boot[510] != 0x55 || boot[511] != 0xAA {
        return None;
    }
    let bps = u16::from_le_bytes([boot[11], boot[12]]);
    let spc = boot[13];
    let num_fats = boot[16];
    let fat_size = u32::from_le_bytes([boot[36], boot[37], boot[38], boot[39]]);
    let root = u32::from_le_bytes([boot[44], boot[45], boot[46], boot[47]]);
    if bps != 512 || spc == 0 || (spc & (spc - 1)) != 0 {
        return None;
    }
    if num_fats == 0 || fat_size == 0 || root < 2 {
        return None;
    }
    let vol_serial = if boot[66] == 0x29 {
        Some(u32::from_le_bytes([boot[67], boot[68], boot[69], boot[70]]))
    } else if boot[67] == 0x29 {
        Some(u32::from_le_bytes([boot[68], boot[69], boot[70], boot[71]]))
    } else {
        None
    };
    let mut vol_label = [0u8; 11];
    vol_label.copy_from_slice(&boot[71..82]);
    Some((vol_serial, vol_label))
}

/// `fs_register(prefix)`: un driver (devfs/console) registra il proprio prefix
/// di mount presso userfs. Ritorna 0 su successo o -1 su errore (anche se
/// userfs non e' ancora pronto: il chiamante puo' ritentare).
#[inline]
pub fn fs_register(prefix: &[u8]) -> i64 {
    fs_register_multi(&[prefix])
}

/// `fs_register_multi(prefixes)`: registra PIU' prefix con UNA SOLA IPC
/// sincrona (Fase 16d). Serve ai driver multi-nodo (devfs: `/dev/null` +
/// `/dev/zero`): due register sincroni consecutivi creerebbero un mount
/// forwardable dopo il primo, e se userfs in quel momento sta inoltrando una
/// richiesta al driver (single-threaded, `send` bloccante) si crea un
/// deadlock incrociato (driver→userfs register, userfs→driver forward).
/// Payload = prefix separati da NUL. Ritorna 0 se TUTTI registrati, -1 se
/// almeno uno fallisce o i buffer non bastano.
pub fn fs_register_multi(prefixes: &[&[u8]]) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    let mut buf = [0u8; 520];
    let mut n = 0usize;
    for (i, p) in prefixes.iter().enumerate() {
        if i > 0 {
            if n + 1 > buf.len() {
                return -1;
            }
            buf[n] = 0;
            n += 1;
        }
        if n + p.len() > buf.len() {
            return -1;
        }
        buf[n..n + p.len()].copy_from_slice(p);
        n += p.len();
    }
    if n == 0 {
        return -1;
    }
    if !req_ring_write(R_REGISTER, n as u64, 0, &buf[..n]) {
        return -1;
    }
    match fs_notify_result(FS_REGISTER, || {
        req_ring_write(R_REGISTER, n as u64, 0, &buf[..n])
    }) {
        Some((result, _, _)) => {
            resp_ring_consume(16);
            fs_reply_val(result)
        }
        None => -1,
    }
}

/// `map_in(chan, phys, virt, count)`: mappa `count` pagine fisiche a partire
/// da `phys` all'indirizzo virtuale `virt` nello spazio del PEER del canale
/// `chan` (ADR-0008). Usato da userfs per iniettare la response ring del client
/// in un driver remoto (devfs/console).
#[inline]
pub fn map_in(chan: u64, phys: u64, virt: u64, count: usize) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_MAP_IN, chan, phys, virt, count as u64) };
    if r < 0 {
        Err(())
    } else {
        Ok(())
    }
}

// ── FS async 1-in-volo (Fase 13) ──────────────────────────────────
//
// La variante async del percorso FS: scrive il request frame, poi `send_async`
// (non bloccante) e ritorna il req_id da raccogliere con `fs_collect`. Il
// formato frame NON ha lunghezza payload esplicita → al massimo 1 operazione
// in volo per processo (`FS_PENDING`): gli altri wrapper FS sincroni e la
// prossima async si rifiutano finche' non si raccoglie. `read_async` supporta
// un singolo round trip (count <= RING_MAX_PAYLOAD), coerente con read_fs che
// spezza le richieste piu' grandi.

/// `read_async(fd, count)`: come `read_fs` (un solo chunk) ma non blocca: scrive
/// il frame `R_READ` nel request ring, notifica userfs con `send_async` e
/// ritorna il `req_id` (>= 1) da passare a `fs_collect`/`fs_collect_msg`.
/// Ritorna -1 se c'e' gia' un'op async in volo, se il request ring e' pieno, o
/// se `send_async` fallisce (backpressure: il frame viene ritirato, nessun
/// frame orfano).
pub fn read_async(fd: i64, count: usize) -> i64 {
    let want = count.min(RING_MAX_PAYLOAD);
    fs_op_async(FS_NOTIFY, R_READ, fd as u64, want as u64, &[])
}

/// `write_async(fd, data)`: come `write_fs` (un solo chunk <= RING_MAX_PAYLOAD)
/// ma non blocca: scrive il frame `R_WRITE` e notifica con `send_async`.
/// Ritorna il `req_id` (>= 1) o -1 (stessi casi di `read_async`). Il payload
/// resta nel request ring per i device remoti (consumato dal driver, come nel
/// percorso sincrono) — la collect legge il result frame come `write_fs`.
pub fn write_async(fd: i64, data: &[u8]) -> i64 {
    fs_op_async(FS_NOTIFY, R_WRITE, fd as u64, data.len() as u64, data)
}

/// `open_async(path, flags)`: come `open` ma non blocca. Ritorna il `req_id`
/// o -1. Da raccogliere con `fs_collect_msg(..., is_read=false)`: fd o -1.
pub fn open_async(path: &str, flags: u32) -> i64 {
    fs_op_async(FS_NOTIFY, R_OPEN, path.len() as u64, flags as u64, path.as_bytes())
}

/// `fs_register_async(prefix)`: come `fs_register` ma non blocca: scrive il
/// frame `R_REGISTER` e notifica con `send_async`. Ritorna il `req_id` o -1.
/// Da raccogliere con `fs_collect_msg(..., is_read=false)`: 0 = registrato.
/// NOTA: a differenza delle altre op FS, la registrazione viaggia sul tag IPC
/// `FS_REGISTER` (non `FS_NOTIFY`): userfs la serve in un handler dedicato.
pub fn fs_register_async(prefix: &[u8]) -> i64 {
    fs_op_async(FS_REGISTER, R_REGISTER, prefix.len() as u64, 0, prefix)
}

/// `fs_buf_reg_async()`: (re)invia gli indirizzi dei ring (handshake
/// `FS_BUF_REG`) con `send_async`, senza frame e senza bloccare. Serve ai
/// driver-server dopo un restart di userfs o un cambio canale: la tabella
/// `rings` di userfs e' indicizzata per canale, quindi sotto un NUOVO canale
/// serve un NUOVO handshake (altrimenti ogni op prende `ERR_NOHANDSHAKE`).
/// Ritorna il `req_id` o -1 (ring mai allocati / op in volo / send fallita).
/// Collect: messaggio con req_id matchato e w0==0 (nessun frame nel ring:
/// NON usare `fs_collect_msg`). Chiama `fs_init()` prima (alloca i ring alla
/// prima volta; le chiamate dopo sono no-op che riusano le pagine).
pub fn fs_buf_reg_async() -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    let req_phys = REQ_PHYS.load(Ordering::Relaxed);
    let resp_phys = RESP_PHYS.load(Ordering::Relaxed);
    if req_phys == 0 || resp_phys == 0 {
        return -1;
    }
    let c = fs_chan();
    if c < 0 {
        return -1;
    }
    match send_async(c as u64, FS_BUF_REG, req_phys, resp_phys) {
        Ok(req) => {
            FS_PENDING.store(req, Ordering::Relaxed);
            req
        }
        Err(_) => {
            FS_CHAN.store(-1, Ordering::Relaxed);
            -1
        }
    }
}

/// Op FS async a basso livello (Fase 15, driver-server): scrive un frame
/// (frame_tag,w0,w1,payload) nel request ring e notifica userfs con
/// `send_async` sul tag IPC `ipc_tag` (`FS_NOTIFY` per le op, `FS_REGISTER`
/// per la registrazione driver).
/// Ritorna il `req_id` (>= 1) o -1 (op gia' in volo / payload troppo grande /
/// ring pieno / send fallita con rollback del frame). Mai bloccante.
/// `read_async`/`write_async` sono wrapper tipizzati; i driver usano questa
/// direttamente per tag senza wrapper (es. `R_REGISTER`).
pub fn fs_op_async(ipc_tag: u64, frame_tag: u32, w0: u64, w1: u64, payload: &[u8]) -> i64 {
    if !fs_init() || fs_async_pending() {
        return -1;
    }
    if payload.len() > RING_MAX_PAYLOAD {
        return -1;
    }
    if !req_ring_write(frame_tag, w0, w1, payload) {
        return -1;
    }
    let c = fs_chan();
    if c < 0 {
        req_ring_rollback(20 + payload.len());
        return -1;
    }
    match send_async(c as u64, ipc_tag, 0, 0) {
        Ok(req) => {
            FS_PENDING.store(req, Ordering::Relaxed);
            req
        }
        Err(_) => {
            // Notifica non consegnata (coda piena = backpressure, o canale
            // morto: peer riavviato e cache FS_CHAN stale): invalida la cache
            // cosi' il prossimo tentativo ri-risolve per nome. Come `fs_send`
            // fa sul path sincrono (Fase 14, init-restart).
            FS_CHAN.store(-1, Ordering::Relaxed);
            // Togli il frame dal request ring.
            req_ring_rollback(20 + payload.len());
            -1
        }
    }
}

/// `fs_collect(req, dst, cap)`: raccoglie la risposta alla `read_async` che ha
/// ritornato `req`. Attende (bloccante, FIFO) la reply async con quel req_id,
/// poi legge il response frame (payload) in `dst`. Ritorna i byte letti, o -1.
/// Resetta il guard 1-in-volo (anche su errore).
/// NOTA (Fase 14): l'attesa filtra per canale (`wait_reply_chan` sul canale FS
/// cachato, stabile per vita del processo): le EXIT_NOTIFY *stale* di altri
/// peer morti vengono saltate, solo la morte del server FS da' -1. Retry
/// automatico e restart del server sono rimandati (serve init-restart).
pub fn fs_collect(req: i64, dst: &mut [u8], cap: usize) -> i64 {
    // Invariante: collect segue una read_async riuscita, che ha risolto e
    // cachato FS_CHAN (>= 0) prima di registrare FS_PENDING.
    let fchan = FS_CHAN.load(Ordering::Relaxed).max(0) as u64;
    let msg = match wait_reply_chan(req, fchan) {
        Ok(m) => m,
        Err(WaitReplyError::ServerDied { pid, code }) => {
            // Server morto mentre attendevamo: niente retry automatico qui
            // (serve init-restart, rimandato); il chiamante vede -1. Azzera i
            // ring: il frame async e' orfano (mai consumato o senza reply) e
            // disallineerebbe le op successive; la prossima op riscrive.
            println!("[libr] fs_collect: server pid {} morto (code {}), req {} perso", pid, code, req);
            unsafe {
                ring_reset(REQ_RING_VA);
                ring_reset(RESP_RING_VA);
            }
            FS_PENDING.store(-1, Ordering::Relaxed);
            return -1;
        }
        Err(_) => {
            FS_PENDING.store(-1, Ordering::Relaxed);
            return -1;
        }
    };
    FS_PENDING.store(-1, Ordering::Relaxed);
    fs_collect_msg(&msg, dst, cap, true)
}

/// `fs_collect_msg(m, dst, cap, is_read)`: raccoglie SENZA BLOCCARE la risposta
/// a una `read_async`/`write_async`/`fs_op_async` il cui messaggio e' GIA'
/// stato ricevuto con `recv_poll` (il chiamante verifica `m.req_id == req` e
/// `m.req_id > 0`). Legge il response frame come `fs_collect` (read, con
/// payload in `dst`) o come `write_fs` (write/result-only: consume + result),
/// resetta il guard 1-in-volo e ritorna byte/result o -1 su errore.
/// Per un driver-server (tty) che non puo' mai bloccarsi: serve le relay DEV
/// nel mentre invece di attendere in `wait_reply` (ciclo userfs<->driver).
pub fn fs_collect_msg(m: &IpcMsg, dst: &mut [u8], cap: usize, is_read: bool) -> i64 {
    FS_PENDING.store(-1, Ordering::Relaxed);
    if is_read {
        if m.w0 == ERR {
            if resp_ring_read().is_some() {
                resp_ring_consume(16);
            }
            return -1;
        }
        match resp_ring_read() {
            Some((result, _w1, payload_len)) => {
                if result == ERR {
                    resp_ring_consume(16);
                    return -1;
                }
                let avail = (result as usize).min(payload_len).min(cap);
                if avail > 0 {
                    resp_ring_read_payload(&mut dst[..avail], avail);
                } else {
                    resp_ring_consume(16);
                }
                avail as i64
            }
            None => fs_reply_val(m.w0),
        }
    } else {
        match resp_ring_read() {
            Some((result, _, _)) => {
                resp_ring_consume(16);
                fs_reply_val(result)
            }
            None => -1,
        }
    }
}

/// Scarta un'op async in volo (Fase 15, driver-server): azzera il guard e i
/// ring (come il path ServerDied di `fs_collect`). Da chiamare quando il
/// server muore (EXIT_NOTIFY) prima di riaprire i peer: frame orfani
/// disallineerebbero le op successive; la prossima op riscrive da zero.
pub fn fs_abort_pending() {
    FS_PENDING.store(-1, Ordering::Relaxed);
    unsafe {
        ring_reset(REQ_RING_VA);
        ring_reset(RESP_RING_VA);
    }
}

// ── CBS bandwidth reservation (Fase 11.4) ──────────────────────────────

/// Informazioni di un server CBS.
pub struct CbsInfo {
    pub budget: u32,
    pub period: u32,
    pub remaining: i32,
}

/// Crea un server CBS con budget Q e periodo P (in tick, 1 tick = 10 ms).
/// Admission control: ritorna l'id del server o `Err(())` se la bandwidth
/// totale supererebbe il cap (~70%).
#[inline]
pub fn cbs_create(budget_ticks: u32, period_ticks: u32) -> Result<i64, ()> {
    let r = unsafe { syscall4(SYS_CBS_CREATE, budget_ticks as u64, period_ticks as u64, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r) }
}

/// Lega il server CBS `server_id` al processo corrente.
#[inline]
pub fn cbs_attach(server_id: i64) -> Result<(), ()> {
    let r = unsafe { syscall4(SYS_CBS_ATTACH, server_id as u64, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(()) }
}

/// Ritorna le informazioni di un server CBS (budget/period/remaining).
#[inline]
pub fn cbs_get_info(server_id: i64) -> Option<CbsInfo> {
    let (rax, rdi, rsi, _, _) = unsafe { syscall4_out(SYS_CBS_GET_INFO, server_id as u64, 0, 0, 0) };
    if rax < 0 {
        None
    } else {
        Some(CbsInfo {
            budget: rax as u32,
            period: rdi as u32,
            remaining: rsi as i32,
        })
    }
}
