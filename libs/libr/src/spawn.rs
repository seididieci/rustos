use super::*;

/// `spawn(name)`: chiede al kernel di creare un nuovo processo dal binario
/// embedded chiamato `name`. Il kernel crea il canale di nascita tra il
/// chiamante (parent) e il figlio: il figlio lo usa come canale 0 (parent), il
/// chiamante riceve qui il channel id per parlare col figlio. Ritorna il
/// channel id; `NoMemory` se il kernel non ha PID/canali (Fase 39).
#[inline]
pub fn spawn(name: &[u8]) -> Result<i64, Error> {
    let pid = unsafe { syscall4(SYS_SPAWN, name.as_ptr() as u64, name.len() as u64, 0, 0) };
    if pid < 0 {
        return Err(Error::NoMemory);
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
/// `io_count == 0` o rifiuto). Ritorna il channel di nascita; `Invalid` se
/// l'immagine/meta sono malformati (pre-validati qui, stesso bound del kernel
/// `SPAWN_IMAGE_MAX`), `NoMemory` se il kernel esaurisce PID/canali/frame
/// (Fase 39: i due rifiuti kernel collassano, la pre-validazione distingue).
#[inline]
pub fn spawn_image(img: &[u8], meta: &SpawnMeta) -> Result<i64, Error> {
    if img.is_empty() || img.len() > SPAWN_IMAGE_MAX {
        return Err(Error::Invalid);
    }
    let c = unsafe {
        syscall4(
            SYS_SPAWN_IMAGE,
            img.as_ptr() as u64,
            img.len() as u64,
            (meta as *const SpawnMeta) as u64,
            core::mem::size_of::<SpawnMeta>() as u64,
        )
    };
    if c < 0 { Err(Error::NoMemory) } else { Ok(c) }
}

/// `exec_image(img)`: sostituisce l'immagine del chiamante con l'ELF `img`
/// (Fase 37, `SYS_EXEC` in-place, senza argv → argc=0). Stesso PID/parent/
/// priorita'/canali; cade l'address space e ne viene caricato uno nuovo;
/// stack nuovo; `image_hash` rimisurato; porte I/O azzerate. NON ritorna mai
/// in caso di successo (salta all'entry della nuova immagine con `rax = 0`);
/// ritorna `Err(())` solo a validazione fallita (processo intatto,
/// completamente utilizzabile). Il FS va ri-fatto lazy: la nuova immagine
/// parte con stato `libr` pristine (BSS azzerato) e `fs_init` rifa' handshake
/// al primo uso. `Invalid` a validazione fallita (Fase 39).
#[inline]
pub fn exec_image(img: &[u8]) -> Result<(), Error> {
    exec_image_args(img, &[])
}

/// `exec_image_args(img, args)`: come `exec_image` ma con argv+env (37.1, env
/// in 43a). `args` = blocco `[argc:8][envc:8][payload NUL-separated]` entro
/// `ARGS_MAX` (normalmente costruito da `exec`/`serialize_argv*`, non a mano);
/// vuoto = argc=0. `Invalid` a validazione fallita (Fase 39).
#[inline]
pub fn exec_image_args(img: &[u8], args: &[u8]) -> Result<(), Error> {
    let r = unsafe {
        syscall4(
            SYS_EXEC,
            img.as_ptr() as u64,
            img.len() as u64,
            args.as_ptr() as u64,
            args.len() as u64,
        )
    };
    // Successo = nessun ritorno (siamo nella nuova immagine); -1 = rifiuto.
    let _ = r;
    Err(Error::Invalid)
}

/// Serializza gli argv nel blocco `[argc:8][envc:8][payload NUL-separated]`
/// per `exec_image_args`/`SYS_EXEC` (Fase 37.1/37.2, env in 43a): `None` se
/// troppi (> 1024) o oltre `ARGS_MAX`. Usato da `exec` e da chi carica prima
/// del fork (la shell: il figlio post-fork ha l'FS avvelenato e non puo' piu'
/// allocare comodo — il parent prepara tutto, il figlio solo esegue).
pub fn serialize_argv(argv: &[&str]) -> Option<alloc::vec::Vec<u8>> {
    serialize_argv_redir_env(argv, &[], &[])
}

/// Come `serialize_argv` ma con spec redirect contrabbandata come ultimo argv
/// (Fase 40.4c/d): una voce per slot attivo (max 3). Formato `REDIR_MAGIC +
/// vfd:noncehex` (grant) o `vfd:@slot` (alias `2>&1`, zero grant) separati da
/// `;`, hex minuscolo senza NUL (il kernel rifiuta code extra e spezza al NUL).
/// Lo startup (`stdio_restore` via `entry!`) la nasconde ad `args_from_stack`:
/// il programma non la vede. `None` a spec invalida o oltre `ARGS_MAX`.
pub fn serialize_argv_redir(
    argv: &[&str],
    spec: &[crate::stdio::RedirEntry],
) -> Option<alloc::vec::Vec<u8>> {
    serialize_argv_redir_env(argv, &[], spec)
}

/// Come `serialize_argv_redir` con in piu' l'environment (Fase 43a): `env` =
/// coppie (nome, valore) serializzate `NAME=val` dopo gli argv (e dopo il
/// magic redirect, che resta l'ULTIMO argv: ordine argv/magic/env, letto
/// cosi' dal kernel). La convenzione `NAME=val` vive qui (il kernel vede byte
/// opachi): voci con nome vuoto o con `=`/NUL nel nome o NUL nel valore sono
/// saltate (mai fail: l'env e' best-effort, gli argv no). `None` se argv/env
/// troppi (> 1024 l'uno) o oltre `ARGS_MAX`.
pub fn serialize_argv_redir_env(
    argv: &[&str],
    env: &[(&str, &str)],
    spec: &[crate::stdio::RedirEntry],
) -> Option<alloc::vec::Vec<u8>> {
    use crate::stdio::RedirEntry;
    if spec.len() > crate::stdio::REDIR_MAX_ENTRIES
        || argv.len() + if spec.is_empty() { 0 } else { 1 } > 1024
        || env.len() > 1024
    {
        return None;
    }
    for e in spec {
        let (v, extra_ok) = match *e {
            RedirEntry::Grant { vfd, .. } => (vfd, true),
            RedirEntry::Alias { vfd, target } => (vfd, target <= 2),
        };
        if v > 2 || !extra_ok {
            return None;
        }
    }
    let mut buf = alloc::vec::Vec::new();
    let argc = argv.len() + if spec.is_empty() { 0 } else { 1 };
    // Conta solo le voci env valide (stessa regola della scrittura sotto).
    let mut envc = 0usize;
    for (k, v) in env {
        if !k.is_empty()
            && !k.as_bytes().iter().any(|&b| b == b'=' || b == 0)
            && !v.as_bytes().iter().any(|&b| b == 0)
        {
            envc += 1;
        }
    }
    buf.extend_from_slice(&(argc as u64).to_le_bytes());
    buf.extend_from_slice(&(envc as u64).to_le_bytes());
    for a in argv {
        buf.extend_from_slice(a.as_bytes());
        buf.push(0);
    }
    // La spec redirect e' l'ULTIMO argv (magic): va subito dopo gli argv,
    // prima degli env (il kernel legge argc stringhe poi envc — l'ordine
    // argv/magic/env e' il contratto).
    if !spec.is_empty() {
        buf.extend_from_slice(crate::stdio::REDIR_MAGIC);
        for (i, e) in spec.iter().enumerate() {
            if i > 0 {
                buf.push(b';');
            }
            match *e {
                RedirEntry::Grant { vfd, nonce } => {
                    buf.push(b'0' + vfd);
                    buf.push(b':');
                    for shift in (0..16).rev() {
                        let nib = ((nonce >> (shift * 4)) & 0xf) as u8;
                        buf.push(if nib < 10 { b'0' + nib } else { b'a' + nib - 10 });
                    }
                }
                RedirEntry::Alias { vfd, target } => {
                    buf.push(b'0' + vfd);
                    buf.push(b':');
                    buf.push(b'@');
                    buf.push(b'0' + target);
                }
            }
        }
        buf.push(0);
    }
    for (k, v) in env {
        if !k.is_empty()
            && !k.as_bytes().iter().any(|&b| b == b'=' || b == 0)
            && !v.as_bytes().iter().any(|&b| b == 0)
        {
            buf.extend_from_slice(k.as_bytes());
            buf.push(b'=');
            buf.extend_from_slice(v.as_bytes());
            buf.push(0);
        }
    }
    if buf.len() as u64 > ARGS_MAX + 16 {
        return None;
    }
    Some(buf)
}
/// `exec(path, argv)`: lancia il programma `path` nell'immagine corrente
/// (Fase 37.1): legge il file via FS, serializza gli argv e chiama
/// `exec_image_args`. Il kernel non tocca mai il FS (ADR-0005). NON ritorna
/// mai in caso di successo; errori nativi (Fase 39: `NotFound` se il file non
/// si carica — dettaglio in Fase 40 —, `TooBig` se gli argv eccedono il bound,
/// `Invalid` a rifiuto del kernel; processo intatto).
#[inline]
pub fn exec(path: &str, argv: &[&str]) -> Result<(), Error> {
    exec_env(path, argv, &[])
}

/// `exec_env(path, argv, env)`: come `exec` con in piu' l'environment (43a).
/// `env` = coppie (nome, valore); stesse regole di `serialize_argv_redir_env`.
#[inline]
pub fn exec_env(path: &str, argv: &[&str], env: &[(&str, &str)]) -> Result<(), Error> {
    let img = match load_file(path) {
        Some(b) if !b.is_empty() => b,
        _ => return Err(Error::NotFound),
    };
    let buf = match serialize_argv_redir_env(argv, env, &[]) {
        Some(b) => b,
        None => return Err(Error::TooBig),
    };
    exec_image_args(&img, &buf)
}

/// `service_register(service)`: occupa lo slot del servizio (ADR-0008). Il
/// chiamante diventa l'owner raggiungibile per nome. `Denied` a rifiuto
/// (Fase 39: gate non-figlio-di-init nel caso comune; slot occupato collassa
/// qui, indistinguibile dal client).
#[inline]
pub fn service_register(service: Service) -> Result<(), Error> {
    let r = unsafe { syscall4(SYS_SERVICE_REGISTER, service as u64, 0, 0, 0) };
    if r < 0 { Err(Error::Denied) } else { Ok(()) }
}

/// `service_lookup(service)`: risolve il servizio in un channel verso
/// l'attuale owner. Ritorna il channel id (>= 0); `NotFound` se non registrato
/// (Fase 39: il caso comune — lookup pre-server — e' preciso).
#[inline]
pub fn service_lookup(service: Service) -> Result<i64, Error> {
    let c = unsafe { syscall4(SYS_SERVICE_LOOKUP, service as u64, 0, 0, 0) };
    if c < 0 { Err(Error::NotFound) } else { Ok(c) }
}

/// Fase 14 (init-restart) — `service_pid(service)`: ritorna il pid
/// dell'attuale owner del servizio; `NotFound` se non registrato (Fase 39).
/// Usato per supervisione/diagnostica (es. verificare che un servizio
/// riavviato sia un processo NUOVO, pid diverso dal precedente).
#[inline]
pub fn service_pid(service: Service) -> Result<i64, Error> {
    let p = unsafe { syscall4(SYS_SERVICE_PID, service as u64, 0, 0, 0) };
    if p < 0 { Err(Error::NotFound) } else { Ok(p) }
}

/// Fase 35 (hardening) — `peer_pid(chan)`: pid del peer del canale `chan`
/// (0 = canale di nascita, come `send`/`recv`); `ServerDied` se il canale non
/// esiste o il peer e' morto (Fase 39). I server lo usano per attribuire una
/// richiesta a un processo (es. la policy `FS_REGISTER` di userfs distingue
/// i figli di init).
#[inline]
pub fn peer_pid(chan: u64) -> Result<i64, Error> {
    let p = unsafe { syscall4(SYS_PEER_PID, chan, 0, 0, 0) };
    if p < 0 { Err(Error::ServerDied) } else { Ok(p) }
}

/// Fase 36 (identita' misurata, Strato 2 di ADR-0026) — `peer_info(chan)`:
/// hash dell'immagine del peer del canale `chan` (0 = canale di nascita);
/// `ServerDied` se il canale non esiste o il peer e' morto (Fase 39). I server
/// lo usano per la policy su identita' (es. userfs accetta il replace di un
/// prefix solo dallo stesso binario; init verifica il manifest pre-spawn).
#[inline]
pub fn peer_info(chan: u64) -> Result<u64, Error> {
    let (rax, rdi, _, _, _) = unsafe { syscall4_out(SYS_PEER_INFO, chan, 0, 0, 0) };
    if rax != 0 { Err(Error::ServerDied) } else { Ok(rdi) }
}

/// Fase 35 (hardening) — `init_bounce(service)`: chiede a init (canale di
/// nascita, solo per figli di init) di uccidere+riavviare il servizio
/// supervisionato `service`. Uccidere un server supervisionato e' operazione
/// da supervisore: i test guidano il caos tramite init invece di killare
/// direttamente (il kill diretto e' parent-scoped). Ritorna il pid ucciso;
/// errori nativi (Fase 39: `ServerDied` se init irraggiungibile, `Failed` se
/// init rifiuta). La morte+restart si osservano poi via `service_pid`.
#[inline]
pub fn init_bounce(service: Service) -> Result<i64, Error> {
    match send(CHANNEL_PARENT, INIT_BOUNCE, service as u64, 0) {
        Ok(r) if r.w0 != u64::MAX => Ok(r.w0 as i64),
        Ok(_) => Err(Error::Failed),
        Err(e) => Err(e),
    }
}

/// `map_physical(phys, virt, count)`: mappa `count` pagine fisiche a partire
/// da `phys` all'indirizzo virtuale `virt` nello spazio del chiamante.
/// Usato dal console server per accedere al frame buffer VGA.
/// `Denied` se il frame non e' mappabile (gate Fase 35, Fase 39).
#[inline]
pub fn map_physical(phys: u64, virt: u64, count: usize) -> Result<(), Error> {
    let r = unsafe { syscall4(SYS_MAP_PHYSICAL, phys, virt, count as u64, 0) };
    if r < 0 {
        return Err(Error::Denied);
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
/// porte/CBS ereditati (solo nascita). `NoMemory` se non c'e' un PID libero o
/// l'OOM colpisce il walk (Fase 39). Nel ramo figlio avvelena automaticamente
/// l'FS (`post_fork_child`): le op FS ritornano `Err` invece di aliasare i ring.
#[inline]
pub fn fork() -> Result<ForkResult, Error> {
    let (rax, rdi, _, _, _) = unsafe { syscall4_out(SYS_FORK, 0, 0, 0, 0) };
    if rax < 0 {
        return Err(Error::NoMemory);
    }
    if rax == 0 {
        crate::fs::session::post_fork_child();
        Ok(ForkResult::Child { chan: CHANNEL_PARENT })
    } else {
        Ok(ForkResult::Parent { pid: rax as u64, chan: rdi })
    }
}
