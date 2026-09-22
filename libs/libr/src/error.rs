//! Errore nativo del sistema (vocabolario condiviso, Fase 39/ADR-0030).
//!
//! Vive in un modulo neutro — NON in `posix::` — perche' lo usano tutti i
//! wrapper, meccanismo e personalita' senza distinzione (`open` come `spawn`,
//! `mmap` come l'IPC). La traduzione in numeri errno POSIX vive SOLO in
//! `posix::to_errno` (bordo): le decisioni usano le varianti, mai i numeri.
//! Regola di stratificazione (vedi `lib.rs`): i moduli meccanismo non usano
//! `posix::`; questo modulo non usa nessuno (solo `ipc::WaitReplyError` per
//! la conversione dalla attesa di reply).

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
