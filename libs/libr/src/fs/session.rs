use super::*;
use crate::*;

static FS_INITED: AtomicBool = AtomicBool::new(false);

/// Guard "1 operazione FS async in volo per processo" (Fase 13): quando e' -1
/// nessuna op async e' in volo; altrimenti contiene il req_id dell'op da
/// raccogliere. I wrapper FS sincroni e il prossimo async si rifiutano
/// (-1) finche' non si raccoglie, per non mischiare frame nel ring (il formato
/// frame non ha lunghezza payload esplicita: un solo frame per volta).
pub(crate) static FS_PENDING: AtomicI64 = AtomicI64::new(-1);

/// Canale verso il fs server (ADR-0008): risolto per nome (`Fs`) alla prima
/// operazione e cachato. `-1` = non ancora risolto.
pub(crate) static FS_CHAN: AtomicI64 = AtomicI64::new(-1);

/// Fisici dei ring per-processo (Fase 14, t28): salvati al primo handshake per
/// poterlo RIPETERE dopo un restart di userfs (le pagine persistono nel
/// processo, ma il nuovo server non conosce la registrazione).
pub(crate) static REQ_PHYS: AtomicU64 = AtomicU64::new(0);
pub(crate) static RESP_PHYS: AtomicU64 = AtomicU64::new(0);

/// Flag "sono un figlio fork" (Fase 34): i ring FS e i canali del padre NON si
/// ereditano (le finestre ring non sono mappate nel figlio). Con questo flag
/// ogni op FS fallisce subito con `Err` invece di faultare sul ring assente o
/// — peggio — di riuscire l'handshake sui phys del padre (aliasing dei ring).
/// Il figlio che deve fare FS deve prima `exec`-care (futuro) o restare senza.
static FS_FORKED: AtomicBool = AtomicBool::new(false);

/// Hook post-fork lato figlio (Fase 34): avvelena l'FS per questo processo e
/// pulisce il guard 1-in-volo copiato in COW dal padre (un'op in volo del
/// padre non e' raccoglibile dal figlio: i ring e il canale sono del padre).
pub fn post_fork_child() {
    FS_FORKED.store(true, Ordering::Relaxed);
    FS_PENDING.store(-1, Ordering::Relaxed);
}

/// True se questo processo e' un figlio fork (FS inutilizzabile).
#[inline]
pub(crate) fn fs_forked() -> bool {
    FS_FORKED.load(Ordering::Relaxed)
}

/// Risolve (una volta) il canale verso il fs server per nome.
pub(crate) fn fs_chan() -> i64 {
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
        if let Ok(chan) = spawn::service_lookup(Service::Fs) {
            FS_CHAN.store(chan, Ordering::Relaxed);
            return chan;
        }
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
    }
}

/// C'e' un'operazione FS async in volo (guad 1-in-volo, Fase 13)?
pub(crate) fn fs_async_pending() -> bool {
    FS_PENDING.load(Ordering::Relaxed) != -1
}

/// Converte il result di una reply FS in i64: `!0` (ERR) → -1.
pub(crate) fn fs_reply_val(w0: u64) -> i64 {
    if w0 == ring::ERR { -1 } else { w0 as i64 }
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
    let t0 = sys::get_ticks();
    let mut last = t0.wrapping_sub(period_ticks);
    loop {
        let now = sys::get_ticks();
        if now.wrapping_sub(last) >= period_ticks {
            last = now;
            if let Some(v) = f() {
                return Some(v);
            }
        }
        if sys::get_ticks() - t0 > bound_ticks {
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
        let fd = sync::open(path, flags);
        if fd >= 0 { Some(fd) } else { None }
    })
    .unwrap_or(-1)
}

/// Risolve il canale verso il fs server con attesa BOUNDED (init-restart):
/// ritenta finche' il servizio si registra o scade il bound. Aggiorna la
/// cache e ritorna il channel, o -1.
pub(crate) fn fs_chan_rt() -> i64 {
    let t0 = sys::get_ticks();
    loop {
        if let Ok(chan) = spawn::service_lookup(Service::Fs) {
            FS_CHAN.store(chan, Ordering::Relaxed);
            return chan;
        }
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
        if sys::get_ticks() - t0 > FS_RELOOKUP_TICKS {
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
pub(crate) fn fs_send(tag: u64, w0: u64, w1: u64) -> Result<IpcReply, ()> {
    if fs_forked() {
        return Err(()); // figlio fork: niente FS (34, mai aliasare i ring)
    }
    let c = fs_chan();
    if c >= 0 {
        if let Ok(r) = ipc::send(c as u64, tag, w0, w1) {
            return Ok(r);
        }
        // Canale morto: invalida e ritenta una volta sola.
        FS_CHAN.store(-1, Ordering::Relaxed);
    }
    let c2 = fs_chan_rt();
    if c2 < 0 {
        return Err(());
    }
    ipc::send(c2 as u64, tag, w0, w1)
}

/// Inizializza (una sola volta) i ring buffer del processo: li alloca col
/// kernel e li registra presso userfs con l'handshake `FS_BUF_REG`.
/// Ritorna false se userfs non e' ancora pronto (il chiamante ritentera').
pub(crate) fn fs_init() -> bool {
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
pub(crate) fn fs_rehandshake() -> bool {
    let req_phys = REQ_PHYS.load(Ordering::Relaxed);
    let resp_phys = RESP_PHYS.load(Ordering::Relaxed);
    if req_phys == 0 {
        return false;
    }
    match fs_send(FS_BUF_REG, req_phys, resp_phys) {
        Ok(_) => {
            FS_INITED.store(true, Ordering::Relaxed);
            unsafe {
                ring_reset(ring::REQ_RING_VA);
                ring_reset(ring::RESP_RING_VA);
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
    if spawn::map_physical(req_phys, ring::REQ_RING_VA, 1).is_err() {
        return false;
    }
    spawn::map_physical(resp_phys, ring::RESP_RING_VA, 1).is_ok()
}

// ── Boot/handshake helpers lato server (A3) ─────────────────────────
// Prima identici nei server userland (devfs/console/kbd + SVC_READY anche in
// fs/tty/disk): differivano solo nella chiamata di registrazione (A3) o nel
// payload w0 (signal_ready).

/// Attende il servizio Fs via soli lookup (spin puri IF=1, mai `get_ticks`
/// che maschera gli interrupt), poi UN tentativo via `register` (nessun frame
/// scritto finche' userfs non c'e': niente spam nel ring che disallineerebbe
/// gli altri client); se fallisce (race: userfs rimorto nel mentre) ricomincia
/// dal lookup. Unbounded come `fs_chan`: senza Fs il driver e' comunque
/// inutile. Idempotente grazie al replace-on-register in userfs.
pub fn ensure_fs_mount(register: fn() -> i64) {
    // Prima i PROPRI ring: le injection map_in di userfs li hanno sovrascritti
    // (stessa VA condivisa, mai ripristinata) — senza remap scriveremmo nelle
    // pagine di un altro client (t28). No-op se mai allocati.
    let _ = fs_remap_self();
    loop {
        while spawn::service_lookup(Service::Fs).is_err() {
            for _ in 0..1_000_000 {
                core::hint::spin_loop();
            }
        }
        if register() == 0 {
            return;
        }
    }
}

/// Segnala SVC_READY al parent in fire-and-forget: a boot init potrebbe non
/// essere ancora in recv (una send sync resterebbe bloccata per sempre), su
/// restart nessuno aspetta. Retry bounded con spin puri, mai hang.
/// `w0` = payload prontezza (1 = pronto; userfs passa `reg_ok`).
pub fn signal_ready(w0: u64) {
    for _ in 0..100 {
        if ipc::send_async(ipc::CHANNEL_PARENT, SVC_READY, w0, 0).is_ok() {
            break;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
    }
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
pub(crate) unsafe fn ring_reset(ring_va: u64) {
    unsafe {
        core::ptr::write_volatile((ring_va + ring::RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((ring_va + ring::RING_TAIL as u64) as *mut u32, 0);
    }
}

/// Notifica un'operazione (`tag` con w0=w1=0; il frame e' gia' scritto nel
/// request ring) e raccoglie il result dal response ring. Con UN redo su
/// `ERR_NOHANDSHAKE`: rifa l'handshake (che AZZERA i ring, scartando gli
/// stale dell'epoca morta) e riscrive il frame corrente via `rewrite`
/// (altrimenti la rinotifica leggerebbe spazzatura/vuoto). Poi UNA rinotifica;
/// al secondo NOHANDSHAKE, -1. Mai loop infiniti, mai doppi frame.
pub(crate) fn fs_notify_result(tag: u64, rewrite: impl Fn() -> bool) -> Option<(u64, u64, usize)> {
    let mut retried = false;
    loop {
        match fs_send(tag, 0, 0) {
            Ok(rep) => {
                if rep.w0 == ring::ERR_NOHANDSHAKE && !retried && fs_rehandshake() && rewrite() {
                    retried = true;
                    continue;
                }
                return ring::resp_ring_read();
            }
            Err(()) => return None,
        }
    }
}
