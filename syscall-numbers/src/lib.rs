#![no_std]

// ── Syscall numbers ────────────────────────────────────────────────────────
// Single source of truth: kernel e userland (libr) dipendono da questo crate.

pub const SYS_EXIT: u64 = 0;
pub const SYS_OPEN: u64 = 3;
pub const SYS_READ: u64 = 4;
pub const SYS_WRITE_FS: u64 = 5;
pub const SYS_CLOSE: u64 = 6;
pub const SYS_READDIR: u64 = 7;
pub const SYS_WRITE: u64 = 2;
pub const SYS_GETPID: u64 = 8;
/// IPC su channel (ADR-0008): `send(channel, tag, w0, w1)` — invia il
/// messaggio al peer del canale e BLOCCA il mittente finche' il peer non fa
/// `reply(tag, w0, w1)` (reply implicita al messaggio corrente). Riusa il
/// numero della vecchia send per PID (16).
pub const SYS_SEND: u64 = 16;
/// `recv()`: riceve il prossimo messaggio dalla propria coda e restituisce
/// `(channel, tag, w0, w1)`. Per le richieste (`req_id >= 0`) `channel` porta
/// il canale sorgente; per le risposte async (`req_id < 0`, Fase 13) porta il
/// `req_id` negativo. Riusa il numero di SYS_RECV (17).
pub const SYS_RECV: u64 = 17;
/// `reply(tag, w0, w1)`: risponde al mittente del messaggio che il chiamante
/// sta correntemente elaborando (reply implicita, `reply_chan` di ADR-0008).
/// Se il peer e' bloccato in `send` la risposta viaggia nel `reply_slot`
/// (sync); se e' async il kernel la accoda con `req_id = -reply_req` (Fase 13).
pub const SYS_REPLY: u64 = 18;
/// IPC async (Fase 13): `send_async(channel, tag, w0, w1)` — come `send` ma
/// NON blocca il mittente. Ritorna il `req_id` assegnato (>= 1), o -1 se la
/// coda del peer e' piena (backpressure) o il canale e' morto.
pub const SYS_SEND_ASYNC: u64 = 33;
/// IPC async (Fase 13): `recv_nonblock()` — come `recv` ma se la coda e'
/// vuota ritorna -1 subito, senza bloccare.
pub const SYS_RECV_NONBLOCK: u64 = 34;
/// Occupa lo slot del servizio `service` (ADRD-0008): il chiamante diventa
/// l'owner del servizio. Fallisce (-1) se il nome e' gia' occupato.
pub const SYS_SERVICE_REGISTER: u64 = 31;
/// Risolve il servizio `service` in un channel verso l'attuale owner.
/// Ritorna il channel id o -1 se il servizio non e' registrato.
pub const SYS_SERVICE_LOOKUP: u64 = 32;
pub const SYS_SPAWN: u64 = 20;
pub const SYS_MAP_PHYSICAL: u64 = 21;
pub const SYS_GET_TICKS: u64 = 22;
pub const SYS_MKDIR: u64 = 23;
pub const SYS_FS_REGISTER: u64 = 24;
pub const SYS_SBRK: u64 = 25;
/// Alloca due pagine fisiche per il ring buffer SPSC del processo corrente
/// (request + response), le mappa a `USER_FS_BUFFER` e `USER_FS_BUFFER+0x1000`,
/// e ritorna gli indirizzi fisici (req in rax, resp in rdi). Il chiamante
/// registra entrambi presso il fs server con una IPC `FS_BUF_REG`.
pub const SYS_RING_ALLOC: u64 = 26;
/// Mappa `count` pagine fisiche a partire da `phys` all'indirizzo virtuale
/// `virt` nello spazio del processo `pid` (usato da userfs per mappare la
/// pagina del client in un driver remoto — devfs/console — a `USER_FS_BUFFER`).
pub const SYS_MAP_IN: u64 = 27;
/// Crea un server CBS (budget, period) → id o -1 (admission control).
pub const SYS_CBS_CREATE: u64 = 28;
/// Lega un server CBS al processo corrente → 0 o -1.
pub const SYS_CBS_ATTACH: u64 = 29;
/// Informazioni CBS (budget/period/remaining) → budget in rax, period in rdi,
/// remaining in rsi, oppure -1.
pub const SYS_CBS_GET_INFO: u64 = 30;
/// Termina un processo user `pid` con il codice `code` (Fase 14, ADR-0010).
/// 0 se il processo e' stato terminato, -1 se il pid non esiste / non e'
/// killabile (init, processi kernel, se stesso).
pub const SYS_KILL: u64 = 35;
/// Ritorna il pid dell'owner attuale del servizio, o -1 se non registrato
/// (Fase 14, init-restart: supervisione e diagnostica).
pub const SYS_SERVICE_PID: u64 = 36;
/// IPC tag: il client ha scritto nel request ring e notifica il server.
pub const FS_NOTIFY: u64 = 0x32;
/// Tag kernel→parent: un figlio e' terminato (exit o kill). Il kernel lo invia
/// sul canale di nascita con `w0` = exit code e `w1` = pid del figlio morto
/// (Fase 14, ADR-0010). Non e' una richiesta: il parent non deve rispondere.
pub const EXIT_NOTIFY: u64 = 0x7C;

// ── Costanti condivise kernel/userland ─────────────────────────────────────
// Pagina fisica scratch riservata dal kernel all'avvio (phys_mem::reserve):
// usata dalla test suite per verificare `map_physical` (aliasing write/read)
// senza toccare memoria di altri processi. 16 MiB: sempre RAM nei config test.
pub const MAP_TEST_PHYS: u64 = 0x1_000000;
pub const MAP_TEST_FRAMES: u64 = 1;

/// Servizi di sistema raggiungibili per nome (ADR-0008, IPC per nome).
/// Il discriminant coincide con l'indice di slot nel registry del kernel
/// (`channels.rs`): `#[repr(u64)]` + assegnazione esplicita per rendere
/// stabile l'ABI attraverso le syscall. Nessuna magic string nel kernel.
///
/// Nota: questo enum elenca SOLO i servizi scopribili per nome. I processi che
/// non sono servizi (helper di test, demo) non registrano nomi: comunicano
/// col parent tramite il canale di nascita creato da `spawn`.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// Console/terminale (VGA + tastiera). Usato da kbd_process e dai client.
    Console = 0,
    /// File system server (userfs): tutti i client FS lo risolvono per nome.
    Fs = 1,
    /// Device file server (devfs, prefix `/dev`).
    Devfs = 2,
    /// Processo init (root della process tree). Non registra attivamente, ma
    /// lo slot esiste per chi deve raggiungerlo (es. test suite).
    Init = 3,
    /// Servizio sacrificale della test suite (Fase 14, t24): un helper lo
    /// registra per farsi raggiungere da un secondo helper via
    /// `service_lookup`, poi viene killato per verificare la notifica
    /// `EXIT_NOTIFY` a TUTTI i peer (non solo al parent). Mai usato in
    /// produzione: nessun server reale lo registra o lo risolve.
    Test = 4,
    /// Driver tastiera PS/2 in userspace (Fase 15, `userkbd`): pubblica
    /// scancode raw sul device `/dev/kbd`. Il kernel (IRQ1) risolve questo
    /// servizio per nome e sveglia l'owner (routing + EOI, mai lettura porte).
    Kbd = 5,
    /// Terminal server in userspace (Fase 15, `usertty`): decodifica i tasti,
    /// fa echo e serve `/dev/input/keyboard`. Registrato per la supervisione
    /// init (restart); nessun altro lo risolve per nome (i client usano il FS).
    Tty = 6,
    /// Disk driver ATA in userspace (Fase 16, `userdisk`): rileva i dischi,
    /// espone `/dev/sdX` (+`/dev/sdXn` per le partizioni MBR). userfs lo
    /// risolve per nome per il data-plane `DISK_*`; init lo supervisiona.
    Disk = 7,
}

/// Massimo numero di servizi conosciuti = dimensione del registro kernel.
pub const SERVICE_COUNT: usize = 8;

/// Canale predefinito del processo: il canale di nascita verso il parent.
/// Ogni processo nasce con canale 0 = parent (o `CHANNEL_NONE` per init/idle).
pub const CHANNEL_PARENT: u64 = 0;
/// Valore "nessun canale" usato quando il parent non esiste.
pub const CHANNEL_NONE: u64 = u64::MAX;

/// Tag del messaggio con cui il kernel sveglia `userkbd` su IRQ1 (Fase 15):
/// bridge interrupt→IPC — un wake senza messaggio non farebbe mai ritorno
/// da `recv()` (la coda vuota ri-blocca in kernel), quindi l'handler accoda
/// questa notify (fire-and-forget, MAI risposta: canale 0, nessun peer) e
/// kbd drena l'hardware ad ogni giro comunque (anche se la notify si perde
/// per coda piena, il drain successivo recupera).
pub const IRQ_NOTIFY_KBD: u64 = 0x41;
