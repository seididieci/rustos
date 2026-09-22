//! Fase 39 (P0, fondamenta posix) — errore nativo del sistema + traduzione POSIX.
//!
//! Il kernel e il wire restano senza concetti POSIX (ADR-0015/0025): niente
//! errno, path o segnali dentro. Gli errori viaggiano come varianti tipate di
//! [`Error`] dentro `libr` e diventano numeri errno SOLO al bordo POSIX, via
//! [`to_errno`] — unico punto di traduzione (table-tested in t53).
//!
//! In Fase 39 esistono solo le varianti di TRASPORTO (fallimenti osservabili
//! senza aiuto del server) piu' i rifiuti che il client puo' attribuire da
//! solo. Le varianti di DOMINIO (`NotFound`, `ReadOnly`, ...) sono dichiarate
//! con mapping fissato ma senza produttori: li aggiunge la Fase 40, quando
//! userfs iniziera' a inviare codici distinti invece del generico `ERR` (che
//! qui collassa in [`Error::Failed`]).

use crate::ipc::WaitReplyError;

/// Errore nativo di un'operazione OS (Fase 39). `Copy` + payload minimi: viaggia
/// per valore nei `Result` senza allocare (hot path IPC/FS invariato).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Server non pronto/raggiungibile: handshake fallito, lookup bound scaduto,
    /// ring mai allocati, retry init-restart esaurito.
    NotReady,
    /// Altra operazione FS in volo (guard 1-in-volo, Fase 13): raccogliere prima.
    Pending,
    /// Richiesta non recapitata: frame non scritto nel ring, coda del peer piena
    /// (backpressure), `send_async` rifiutata. In Fase 39 "coda piena" e "canale
    /// morto" sono indistinguibili dal client: collassano qui.
    RingFull,
    /// Il peer che doveva rispondere e' morto (EXIT_NOTIFY osservata). Senza
    /// payload: chi serve pid/code usa `WaitReplyError` direttamente.
    ServerDied,
    /// Rifiuto di policy senza dettaglio attribuibile dal client (gate
    /// non-figlio-di-init su kill/register/map, porte negate, ...).
    Denied,
    /// Risorse esaurite: PID/canali/pagine (spawn, fork, mmap, sbrk, DMA, ...).
    NoMemory,
    /// Risorsa occupata (slot servizio occupato, mount con fd aperti, CBS cap).
    /// In Fase 39 alcuni rifiuti indistinguibili (es. register: occupato vs
    /// gate) collassano in `Denied`; qui solo l'occupazione certa.
    Busy,
    /// Argomenti o validazione rifiutati (exec oltre bound, meta invalide,
    /// munmap/mprotect parziali, frame malformati, messaggi fuori ordine).
    Invalid,
    /// Rifiuto del server senza dettaglio (Fase 39: userfs risponde solo `ERR`;
    /// la Fase 40 tipizzera' questi casi producendo le varianti di dominio).
    Failed,
    // ── Dominio FS: mapping fissato qui, produttori in Fase 40 ──
    /// Path inesistente.
    NotFound,
    /// Path non directory dove serviva una directory.
    NotDir,
    /// Path directory dove serviva un file.
    IsDir,
    /// Esiste gia' (O_EXCL futuro, mkdir su esistente, ...).
    Exists,
    /// Scrittura/mutazione su volume read-only (FAT senza permessi, ...).
    ReadOnly,
    /// Oltre il limite di dimensione (spawn_image/exec bound, file troppo grosso).
    TooBig,
}

// ── Numeri errno POSIX standard (solo per `to_errno`, mai nel kernel/wire) ──
pub const EPERM: i64 = 1;
pub const ENOENT: i64 = 2;
pub const EIO: i64 = 5;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EBUSY: i64 = 16;
pub const EEXIST: i64 = 17;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const EFBIG: i64 = 27;
pub const EROFS: i64 = 30;
pub const EAGAIN: i64 = 35;

/// UNICA traduzione nativo→errno (Fase 39). Totale sul dominio (il compilatore
/// impone un braccio per variante: nessuna nuova variante senza errno). Il caso
/// peggiore di un bug qui e' un numero sbagliato in un messaggio, mai un
/// comportamento errato del sistema (le decisioni usano le varianti, non i numeri).
pub fn to_errno(e: Error) -> i64 {
    match e {
        Error::NotReady => EIO,
        Error::Pending => EAGAIN,
        Error::RingFull => EAGAIN,
        Error::ServerDied => EIO,
        Error::Denied => EACCES,
        Error::NoMemory => ENOMEM,
        Error::Busy => EBUSY,
        Error::Invalid => EINVAL,
        Error::Failed => EIO,
        Error::NotFound => ENOENT,
        Error::NotDir => ENOTDIR,
        Error::IsDir => EISDIR,
        Error::Exists => EEXIST,
        Error::ReadOnly => EROFS,
        Error::TooBig => EFBIG,
    }
}

impl From<WaitReplyError> for Error {
    /// Collassa l'errore di `wait_reply` nel nativo (perde pid/code: chi li
    /// serve matcha `WaitReplyError` direttamente invece di convertire).
    fn from(e: WaitReplyError) -> Error {
        match e {
            WaitReplyError::ServerDied { .. } => Error::ServerDied,
            WaitReplyError::RecvFailed => Error::ServerDied,
            WaitReplyError::UnexpectedMsg => Error::Invalid,
        }
    }
}
