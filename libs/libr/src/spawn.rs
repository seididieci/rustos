use super::*;

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

/// Metadati di `spawn_image` (Fase 21, servizi da disco): layout `repr(C)` da
/// 40 B, identico allo `SpawnMeta` kernel (validato per size). Nome NUL-padded
/// (non vuoto, stampabile); `prio` 1..31; fino a 4 range I/O (start <= end);
/// `flags` (Fase 22: solo `SPAWN_FLAG_DETACH`, resto riservato = 0).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct SpawnMeta {
    pub name: [u8; 16],
    pub prio: u8,
    pub io_count: u8,
    pub flags: u8,
    pub _pad: [u8; 5],
    pub io_ranges: [(u16, u16); 4],
}

impl SpawnMeta {
    /// Costruisce i metadati da nome/priorita'/porte (tronca il nome a 16,
    /// NUL-padded; piu' di 4 range → i primi 4? No: troppi → None, fail-loud).
    /// Flags a 0 (attached: cascata di morte normale).
    pub fn new(name: &str, prio: u8, io: &[(u16, u16)]) -> Option<Self> {
        if name.is_empty() || io.len() > 4 {
            return None;
        }
        let mut m = SpawnMeta {
            name: [0u8; 16],
            prio,
            io_count: io.len() as u8,
            flags: 0,
            _pad: [0u8; 5],
            io_ranges: [(0, 0); 4],
        };
        let bytes = name.as_bytes();
        let n = bytes.len().min(16);
        m.name[..n].copy_from_slice(&bytes[..n]);
        m.io_ranges[..io.len()].copy_from_slice(io);
        Some(m)
    }

    /// Marca il figlio come detached (Fase 22): alla morte del parent viene
    /// ri-parentato a init invece di terminare in cascata. Scelta dello
    /// spawner (builder: il figlio non puo' auto-staccarsi), irrevocabile.
    pub fn detached(mut self) -> Self {
        self.flags |= SPAWN_FLAG_DETACH;
        self
    }
}

/// `spawn_image(img, meta)`: come `spawn` ma il binario e' letto dalla memoria
/// del chiamante (Fase 21, servizi da disco e helper di test). Primitiva
/// generale: le porte I/O restano privilegio di init (pid 1, gli altri con
/// `io_count == 0` o rifiuto). Ritorna il channel di nascita o `Err`.
#[inline]
pub fn spawn_image(img: &[u8], meta: &SpawnMeta) -> Result<i64, ()> {
    let c = unsafe {
        syscall4(
            SYS_SPAWN_IMAGE,
            img.as_ptr() as u64,
            img.len() as u64,
            (meta as *const SpawnMeta) as u64,
            core::mem::size_of::<SpawnMeta>() as u64,
        )
    };
    if c < 0 { Err(()) } else { Ok(c) }
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

/// Fase 35 (hardening) — `peer_pid(chan)`: pid del peer del canale `chan`
/// (0 = canale di nascita, come `send`/`recv`), o `Err`. I server lo usano
/// per attribuire una richiesta a un processo (es. la policy `FS_REGISTER` di
/// userfs distingue i figli di init).
#[inline]
pub fn peer_pid(chan: u64) -> Result<i64, ()> {
    let p = unsafe { syscall4(SYS_PEER_PID, chan, 0, 0, 0) };
    if p < 0 { Err(()) } else { Ok(p) }
}

/// Fase 36 (identita' misurata, Strato 2 di ADR-0026) — `peer_info(chan)`:
/// hash dell'immagine del peer del canale `chan` (0 = canale di nascita),
/// o `Err` se il canale non esiste/il peer e' morto. I server lo usano per
/// la policy su identita' (es. userfs accetta il replace di un prefix solo
/// dallo stesso binario; init verifica il manifest prima dello spawn).
#[inline]
pub fn peer_info(chan: u64) -> Result<u64, ()> {
    let (rax, rdi, _, _, _) = unsafe { syscall4_out(SYS_PEER_INFO, chan, 0, 0, 0) };
    if rax != 0 { Err(()) } else { Ok(rdi) }
}

/// Fase 35 (hardening) — `init_bounce(service)`: chiede a init (canale di
/// nascita, solo per figli di init) di uccidere+riavviare il servizio
/// supervisionato `service`. Uccidere un server supervisionato e' operazione
/// da supervisore: i test guidano il caos tramite init invece di killare
/// direttamente (il kill diretto e' parent-scoped). Ritorna il pid ucciso o
/// `Err` (servizio ignoto / init irraggiungibile). La morte+restart si
/// osservano poi via `service_pid` come prima.
#[inline]
pub fn init_bounce(service: Service) -> Result<i64, ()> {
    match send(CHANNEL_PARENT, INIT_BOUNCE, service as u64, 0) {
        Ok(r) if r.w0 != u64::MAX => Ok(r.w0 as i64),
        _ => Err(()),
    }
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

/// Esito di `fork()` (Fase 34): nel padre il pid del figlio + il canale di
/// nascita (stesso id da entrambi i lati; il padre lo usa numerico, il figlio
/// come canale 0 = `CHANNEL_PARENT`); nel figlio solo il canale di nascita
/// (il pid del figlio lo sa il padre, il figlio sa di essere il figlio).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkResult {
    /// Padre: pid del figlio + canale di nascita verso di lui.
    Parent { pid: u64, chan: u64 },
    /// Figlio: canale di nascita verso il padre (= canale 0).
    Child { chan: u64 },
}

/// `fork()`: duplica il chiamante in COW (address space condiviso, copie
/// private al primo write). Il figlio riprende come ritorno dalla syscall con
/// 0; priorita' e `req_next` ereditati (e divergono), niente canali/fd/ring/
/// porte/CBS ereditati (solo nascita). `Err` se non c'e' un PID libero o
/// l'OOM colpisce il walk. Nel ramo figlio avvelena automaticamente l'FS
/// (`post_fork_child`): le op FS ritornano `Err` invece di aliasare i ring.
#[inline]
pub fn fork() -> Result<ForkResult, ()> {
    let (rax, rdi, _, _, _) = unsafe { syscall4_out(SYS_FORK, 0, 0, 0, 0) };
    if rax < 0 {
        return Err(());
    }
    if rax == 0 {
        crate::fs::session::post_fork_child();
        Ok(ForkResult::Child { chan: CHANNEL_PARENT })
    } else {
        Ok(ForkResult::Parent { pid: rax as u64, chan: rdi })
    }
}
