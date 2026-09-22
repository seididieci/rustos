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
/// (request + response), le mappa a `USER_FS_BUFFER` e `USER_RESP_RING`
/// (= `USER_FS_BUFFER+0x10000`), e ritorna gli indirizzi fisici (req in rax,
/// resp in rdi). Il chiamante registra entrambi presso il fs server con una
/// IPC `FS_BUF_REG`. Ogni chiamata da' pagine fresche (multi-coppia, Fase 16).
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
/// Snapshot `ps` del processo `pid` (Fase 19.1): 0 se lo slot e' vivo, -1 se
/// vuoto/terminato. Campi multi-registro (pattern `CBS_GET_INFO`): nome (16 B,
/// il piu' lungo oggi e' "userdevreader"=13) in rdi+rsi (LE), `rdx` packed,
/// `r10` = tick consumati. Layout `rdx`: bit 0-7 stato (0=Ready, 1=Blocked),
/// 8-15 prio (0-31), 16-23 parent+1 (0=nessuno), 24-31 ipc (0=None, 1=OnRecv,
/// 2=OnReply). Il chiamante marca "run" il proprio pid (da `getpid`).
pub const SYS_PS_INFO: u64 = 37;
/// Spawna un processo dal binario in memoria del chiamante (Fase 21, servizi
/// da disco): `(img_ptr, img_len, meta_ptr, meta_len)`. Primitiva generale
/// (come fork+exec): le porte I/O sono privilegio root (solo pid 1, gli altri
/// con `io_count == 0`); prio 1..31 per tutti. `meta` e' uno SpawnMeta da 40 B
/// (vedi sotto); ritorna il channel di nascita o -1.
pub const SYS_SPAWN_IMAGE: u64 = 38;
/// Mappa `len` byte anonimi privati (zero-fill lazy) nel basso canonico
/// (Fase 28, mmap): `(hint, len, prot, flags)`. Ritorna la base o -1.
/// Solo anonimo in 28: `prot` deve essere `PROT_READ|PROT_WRITE`, `flags`
/// 0 (hint consigliato, 0 = scelta kernel) o `MMAP_FIXED` (hint obbligatorio).
pub const SYS_MMAP: u64 = 39;
/// Smappa `[addr, addr+len)`: solo VMA intere in 28 (parziali = -1 senza
/// cambiare stato). Ritorna 0 o -1.
pub const SYS_MUNMAP: u64 = 40;
/// Cambia le protezioni di `[addr, addr+len)` (Fase 29, mprotect):
/// `(addr, len, prot)`. Solo VMA intere (come `munmap`). Ritorna 0 o -1.
pub const SYS_MPROTECT: u64 = 41;
/// Crea una regione di memoria condivisa (Fase 30): `(len)` → id (>= 1) o -1.
pub const SYS_SHM_CREATE: u64 = 42;
/// Mappa una regione condivisa (Fase 30): `(id, hint, prot, flags)` → base o
/// -1. Le pagine sono le stesse per tutti i mappatori (zero-copy tra processi).
pub const SYS_SHM_MAP: u64 = 43;
/// Contatori shared text (Fase 32, debug/test): ritorna `hits` in rax,
/// `misses` in rdi, `live` in rsi (nessun argomento).
pub const SYS_TEXT_STATS: u64 = 44;
/// Crea un figlio che condivide l'address space del chiamante in COW
/// (Fase 34, `fork`): nessun argomento. Ritorna al padre `(pid_figlio,
/// canale_nascita)` (pid in rax, canale in rdi via multi-registro), al figlio
/// `(0, canale_nascita)` (il figlio usa il canale 0 = `CHANNEL_PARENT`); -1
/// se non c'e' un PID libero o il pool canali e' esaurito.
pub const SYS_FORK: u64 = 45;
/// `peer_pid(chan)`: pid del peer del canale `chan` (0 = canale di nascita,
/// come `send`/`recv`), o -1 se il canale non esiste/`chan` non ne fa parte
/// (Fase 35, hardening: i server possono attribuire una richiesta a un
/// processo — es. la policy `FS_REGISTER` di userfs distingue i figli di
/// init). Non rivela nulla che `ps_info` non mostri gia'.
pub const SYS_PEER_PID: u64 = 46;
/// `peer_info(chan)`: hash dell'immagine (`image_hash`, FNV-1a sull'ELF) del
/// peer del canale `chan` (0 = canale di nascita), o -1 se il canale non
/// esiste/il peer e' morto (Fase 36, identita' misurata, Strato 2 di ADR-0026).
/// Multi-registro (pattern `PS_INFO`): rax = 0 + rdi = hash; -1 = errore.
/// I server lo usano per la policy su identita' (manifest init, `FS_REGISTER`
/// in userfs). Non rivela nulla oltre l'identita' del binario (nomi e pid sono
/// gia' visibili via `ps_info`/`peer_pid`).
pub const SYS_PEER_INFO: u64 = 47;
/// Sostituisce l'immagine del chiamante (Fase 37, `exec` in-place):
/// `(img_ptr, img_len)` = ELF in memoria del chiamante (mai il FS: il kernel
/// non tocca il disco, ADR-0005). Stesso PID/parent/priorita'/canali (fd
/// server-side sopravvivono); cade tutto l'address space e ne viene caricato
/// uno nuovo; stack nuovo (argc=0 in 37.0, argv in 37.1); `image_hash`
/// rimisurato (Strato 2); porte I/O azzerate. Non ritorna mai al chiamante
/// (salta all'entry nuova); -1 = validazione fallita, processo intatto.
/// Stesso bound di `spawn_image` (256 KiB).
pub const SYS_EXEC: u64 = 48;
/// Alloca `pages` (1..=DMA_PAGES_MAX) frame fisici CONTIGUI azzerati per DMA
/// Bus-Master (Fase 38.1, ATA DMA): li mappa RW/NX a `USER_DMA_VA` e ritorna
/// il fisico base (il chiamante programma PRD e BMIBA con phys reali — VA
/// non bastano al device). Single-slot per processo (seconda alloc = -1);
/// free a teardown/exec, mai ereditata dal fork. Precedente: `SYS_RING_ALLOC`
/// (26) ritorna gia' phys alle ring — stessa neutralita' (ADR-0005: il kernel
/// non tocca il disco, alloca solo frame).
pub const SYS_DMA_ALLOC: u64 = 49;
/// Cap pagine di `SYS_DMA_ALLOC` (38.1: 1 pagina = PRD + 7 settori bastano).
pub const DMA_PAGES_MAX: usize = 4;
/// Bound del blocco argv serializzato (Fase 37.1): `[argc:8][payload
/// NUL-separated]` oltre cui `exec` rifiuta fail-loud. Single source
/// kernel+user (`libr` lo riesporta): 8 KiB bastano a shell e test con margine.
pub const ARGS_MAX: u64 = 8 * 1024;
/// Immagine massima spawabile/eseguibile (Fase 21: 64 frame = 256 KiB; i binari
/// sono < 70 KiB — un singolo spawn non puo' svuotare il pool frame). Single
/// source kernel (`sys_spawn_image`, stesso bound per `exec` in Fase 37) +
/// user (`libr` pre-valida prima della syscall per errori precisi).
pub const SPAWN_IMAGE_MAX: usize = 256 * 1024;
/// Protezioni `mmap`/`mprotect` (29: NONE/R/RW con enforcement; W solo e
/// PROT_EXEC rifiutati — eseguibile solo il codice di spawn).
pub const PROT_NONE: u64 = 0x0;
pub const PROT_READ: u64 = 0x1;
pub const PROT_WRITE: u64 = 0x2;
/// Flag `mmap`: piazza esattamente a `hint` (o fallisci), niente fallback.
pub const MMAP_FIXED: u64 = 0x1;
/// Flag `shm_map` (Fase 33, COW): mappa i frame della regione `RO`+`COW`
/// (copy-on-write) invece che condivisi-scrittibili. Solo con
/// `prot == PROT_READ` (la scrittura materializza la copia privata al primo
/// fault). Ogni mappatura COW incrementa il refcount per-frame: due processi
/// leggono gli stessi dati finche' non scrivono, poi isolati.
pub const MAP_COW: u64 = 0x2;
/// Layout stack user condiviso kernel+test (Fase 29, single source qui):
/// lo stack vive a `USER_STACK_TOP` (cresce verso il basso, 4 frame);
/// la pagina a `USER_STACK_GUARD` (subito sotto) NON e' mai mappata:
/// lo stack overflow fa #PF li' → kill (mai corruzione silenziosa).
/// NB: `USER_BASE + 0x400_000` (0x0000_4000_0040_0000).
pub const USER_STACK_TOP: u64 = 0x0000_4000_0040_0000;
pub const USER_STACK_FRAMES: usize = 4;
pub const USER_STACK_GUARD: u64 = 0x0000_4000_0040_0000 - 5 * 0x1000;
/// Codice di uscita con cui il kernel termina un processo user che provoca
/// un fault di memoria non recuperabile (Fase 29: #PF di protezione, accesso
/// a PROT_NONE, stack overflow nella guard). Il parent lo osserva via
/// EXIT_NOTIFY (w0). 139 = 128 + 11 (SIGSEGV, convenzione POSIX).
pub const FAULT_EXIT_CODE: i64 = 139;
/// Base del codice user (link address del binario ELF, single source
/// kernel+test): il loader ELF mappa i segmenti al `p_vaddr` di link e
/// l'entry e' `e_entry`. Prima solo nel kernel (`layout.rs`).
pub const USER_CODE: u64 = 0x0000_4000_0000_0000;
/// Finestra staging DMA del processo corrente (Fase 38.1, single source
/// kernel+user come `USER_CODE`): `SYS_DMA_ALLOC` mappa qui i frame contigui
/// (PRD + dati). Dopo CLI_RESP (`+0x230_000`) e DISK_RESP (`+0x250_000`):
/// prima VA libera.
pub const USER_DMA_VA: u64 = 0x0000_4000_0026_0000;
/// Flag `SpawnMeta.flags` (Fase 22, detach): il figlio non partecipa alla
/// cascata di morte del parent — alla morte del parent viene ri-parentato a
/// init invece di terminare. Deciso dallo spawner (il figlio non puo'
/// auto-staccarsi); irrevocabile. Bit riservati: devono essere 0 (rifiuto).
pub const SPAWN_FLAG_DETACH: u8 = 0x01;
/// Bound di scansione per `ps` (Fase 19.1): i PID vivono in 0..PS_SCAN_MAX.
/// Deve restare uguale al `MAX_PIDS` del kernel (32, Fase 14).
pub const PS_SCAN_MAX: u32 = 32;
/// IPC tag: il client ha scritto nel request ring e notifica il server.
pub const FS_NOTIFY: u64 = 0x32;
// ── Tag IPC userland, single source (centralizzazione DocsB: prima duplicati
// in `libr`, userfs/userdisk/init/tty/kbd e come letterali nei test) ────────
// - FS_REGISTER (0x30): un driver registra il prefix di mount (frame
//   R_REGISTER nel request ring, letto da userfs).
// - FS_BUF_REG (0x31): handshake register-only "i miei ring sono req=w0,
//   resp=w1" (client e driver verso userfs).
// - KBD_NOTIFY (0x40): userkbd → usertty, scancode in coda (w0 = count).
// - SVC_READY (0x7D): servizio → init, "sono su" (fire-and-forget a boot).
// - TEST_DONE (0x7E): test suite → init, fine sequenza (w0 = ok count).
// - INIT_BOUNCE (0x7F): figlio → init, "uccidi+riavvia il servizio `w0`"
//   (Fase 35, hardening: uccidere un server supervisionato e' operazione da
//   supervisore — i test guidano il caos tramite init invece di killare
//   direttamente; init risponde con reply (0 = pid ucciso, ERR = ignoto)).
pub const FS_REGISTER: u64 = 0x30;
pub const FS_BUF_REG: u64 = 0x31;
pub const KBD_NOTIFY: u64 = 0x40;
pub const SVC_READY: u64 = 0x7D;
pub const TEST_DONE: u64 = 0x7E;
pub const INIT_BOUNCE: u64 = 0x7F;
// ── Protocollo DEV_* (userfs→driver: devfs/console/kbd/tty/disk, DocsD) ───
// Single source of truth dei tag e dei device type (w0 di DEV_OPEN): prima
// duplicati in userfs/userdisk/devfs/console/kbd/tty. userfs instrada per
// prefix al server e inoltra l'op; il driver risponde sul relay.
// - OPEN/READ/WRITE/CLOSE/READDIR: op sui nodi device (raw o sintetizzati).
// - Type: NULL/ZERO (devfs), KEYBOARD (tty, `/dev/input`), CONSOLE (console,
//   `/dev/console`), KBD (kbd, `/dev/kbd`); userdisk usa handle disco<<16|sub.
pub const DEV_OPEN: u64 = 0x20;
pub const DEV_READ: u64 = 0x21;
pub const DEV_WRITE: u64 = 0x22;
pub const DEV_CLOSE: u64 = 0x23;
pub const DEV_READDIR: u64 = 0x24;
pub const DEV_NULL: u64 = 0;
pub const DEV_ZERO: u64 = 1;
pub const DEV_KEYBOARD: u64 = 2;
pub const DEV_CONSOLE: u64 = 3;
pub const DEV_KBD: u64 = 4;
/// Tag kernel→parent: un figlio e' terminato (exit o kill). Il kernel lo invia
/// sul canale di nascita con `w0` = exit code e `w1` = pid del figlio morto
/// (Fase 14, ADR-0010). Non e' una richiesta: il parent non deve rispondere.
pub const EXIT_NOTIFY: u64 = 0x7C;

// ── Protocollo DISK_* (data-plane userfs→userdisk, Fase 16) ───────────────
// Single source of truth dei tag (Fase 16c): prima duplicati in
// `userland/fs/src/ipc_disk.rs` e `userland/disk/src/main.rs`. I tag viaggiano
// nei registri IPC; i payload (nomi, settori) nei ring dedicati.
//
// Canale diretto userfs→userdisk (service_lookup(Disk)):
// - HELLO/OPEN/CLOSE: solo registri, niente frame.
// - READ: un settore per chiamata, frame `[512:8][0:8][settore]` nel ring DISK_RESP.
// - WRITE (20): un settore per chiamata, frame `[512:8][settore]` nel ring
//   DISK_REQ (handle in w0, lba in w1 dei registri); reply w0 = 0 o ERR,
//   nessun frame di risposta.
// - RESOLVE (16c): il nome nodo ("sda", "sda1") viaggia in un frame
//   `[namelen:8][name]` nel ring DISK_REQ; la reply porta l'handle in w0
//   (o ERR). userdisk e' l'unico proprietario della mappa nome→handle:
//   userfs non indovina piu' nulla dal nome.
pub const DISK_HELLO: u64 = 0x50;
pub const DISK_OPEN: u64 = 0x51;
pub const DISK_READ: u64 = 0x52;
pub const DISK_CLOSE: u64 = 0x53;
pub const DISK_RESOLVE: u64 = 0x54;
/// Scrive un settore (Fase 20, FAT scrivibile): vedi sopra.
pub const DISK_WRITE: u64 = 0x55;

// ── Tag delle operazioni FS (nel frame del ring, non nell'IPC) ────────────
// Single source of truth (Fase 17): prima duplicati in `libr`, `userfs` e
// (R_REGISTER) `userdisk`. Il formato frame e' `[tag:4][w0:8][w1:8][payload]`.
pub const R_OPEN: u32 = 0x10;
pub const R_READ: u32 = 0x11;
pub const R_WRITE: u32 = 0x12;
pub const R_CLOSE: u32 = 0x13;
pub const R_READDIR: u32 = 0x14;
pub const R_MKDIR: u32 = 0x15;
/// Monta una sorgente su un target: payload "source\0target\0".
pub const R_MOUNT: u32 = 0x16;
/// Smonta un target: payload "target".
pub const R_UMOUNT: u32 = 0x17;
/// Cancella un file o una directory VUOTA: payload = path (Fase 18.2).
/// Solo ramfs (su FAT manca l'unlink e i device remoti rifiutano con ERR).
pub const R_DELETE: u32 = 0x1A;
/// Metadati del path (Fase 19.2, zero kernel): payload = path; risposta
/// self-written `[size:8][kind:8]`, payload vuoto. Nessun fd coinvolto.
pub const R_STAT: u32 = 0x1B;
/// `kind` per R_STAT (Fase 19.2): bit 0-1 tipo + bit 7 readonly.
pub const STAT_FILE: u64 = 0;
pub const STAT_DIR: u64 = 1;
pub const STAT_DEVICE: u64 = 2;
pub const STAT_READONLY: u64 = 0x80;
/// Flag `open`: crea il file se non esiste (Fase 18.2: prima l'open creava
/// sempre su ramfs ignorando i flag — ora POSIX: senza O_CREAT il file deve
/// esistere). Viaggia in w1 del frame R_OPEN (libr lo passava gia', il server
/// lo ignorava).
pub const O_CREAT: u32 = 0x200;
/// Un driver registra il proprio prefix di mount.
pub const R_REGISTER: u32 = 0x30;
/// Riduce i propri diritti sul canale (Fase 17, self-restriction only):
/// w0 = mask dei bit da TENERE (solo shrink: new = old & w0), w1 = len
/// subtree, payload = subtree (vuoto = solo-ops). Mai widen, mai auth.
pub const R_RIGHTS_DROP: u32 = 0x18;
/// Legge i propri diritti (Fase 17): niente payload; risposta self-written
/// `[ops:8][sublen:8][subtree]` (subtree normalizzato, "" = root).
pub const R_RIGHTS_GET: u32 = 0x19;

// ── Bit dei diritti per-canale lato userfs (Fase 17, 18.2) ───────────────
// Solo riduzione (DROP fa AND), default ALL. CLOSE sempre consentito (rilascia
// stato, mai escalation: nessun bit). Diritti effimeri: restart userfs =
// re-handshake full; niente policy per-identita' (serve il kernel).
pub const RIGHTS_OPEN: u32 = 0x01;
pub const RIGHTS_READ: u32 = 0x02;
pub const RIGHTS_WRITE: u32 = 0x04;
pub const RIGHTS_READDIR: u32 = 0x08;
pub const RIGHTS_MKDIR: u32 = 0x10;
pub const RIGHTS_MOUNT: u32 = 0x20;
pub const RIGHTS_UMOUNT: u32 = 0x40;
/// Cancellazione file/dir vuote (Fase 18.2, `R_DELETE`).
pub const RIGHTS_DELETE: u32 = 0x80;
pub const RIGHTS_ALL: u32 = 0xFF;

// ── Costanti condivise kernel/userland ─────────────────────────────────────
// Pagina fisica scratch riservata dal kernel all'avvio (phys_mem::reserve):
// usata dalla test suite per verificare `map_physical` (aliasing write/read)
// senza toccare memoria di altri processi. 64M: oltre immagine, heap, bitmap
// e tabelle statiche (16M) in ogni config; sempre RAM nei config test.
// (Prima a 16M: collideva con le tabelle `.tables_high` dell'higher-half 27.3
// — t12 scriveva pattern sopra le PD direct → fault ritardato. Mai piu':
// invariante di non-sovrapposizione verificata dal compilatore in kernel.)
pub const MAP_TEST_PHYS: u64 = 0x4_000000;
/// Frame scratch riservati (2 pagine: P1 e P2 di t29/maphammer — il
/// data-plane di test usa due frame distinti sulla stessa VA in processi
/// diversi). Fase 35: solo questi frame + ring + VGA sono mappabili via
/// `map_physical`; con 1 frame il secondo scratch veniva rifiutato.
pub const MAP_TEST_FRAMES: u64 = 2;

/// Identita' misurata di un'immagine ELF (Fase 36, Strato 2 di ADR-0026):
/// FNV-1a a 64 bit sui byte dell'ELF. Single source kernel+user: il kernel la
/// misura allo spawn (`image_hash` nel PCB) e la espone via `SYS_PEER_INFO`;
/// init/userfs la ricalcolano sui byte caricati per la policy (manifest,
/// `FS_REGISTER`). Stesso algoritmo dello sharing text (Fase 32, che ora usa
/// questa funzione): sui `.bin` di build i due valori coincidono bit-per-bit.
pub fn image_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

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
    /// Server di personalita' POSIX in userspace (Fase 39, P0 della roadmap
    /// 39-45, ADR-0030): tabelle fd virtuali, pipe, job control. Solo
    /// controllo e stato globale POSIX; il kernel resta neutro (ADR-0025) e
    /// il data plane resta diretto client→userfs.
    Posix = 8,
}

/// Massimo numero di servizi conosciuti = dimensione del registro kernel.
/// Fase 39: 8→16 (slot 9-15 liberi per futuri servizi senza ritoccare il
/// kernel; discriminant 0-7 storici intoccati, ABI stabile).

/// Massimo numero di servizi conosciuti = dimensione del registro kernel.
pub const SERVICE_COUNT: usize = 16;

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
/// Tag del messaggio con cui il kernel sveglia `userdisk` su IRQ14/15 (Fase 38,
/// ATA DMA: bridge interrupt→IPC come IRQ1→kbd — un wake senza messaggio non
/// farebbe mai ritorno da `recv()`; userdisk drena lo status Bus-Master ad ogni
/// giro, anche su wake spurio o notify persa per coda piena). Fire-and-forget,
/// MAI risposta: canale 0, nessun peer.
pub const IRQ_NOTIFY_DISK: u64 = 0x42;
