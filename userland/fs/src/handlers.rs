use super::*;

// ── Handler (Option<u64> internamente) ─────────────────────────────

pub fn handle_open(
    fs: &mut ramfs::RamFs,
    ftable: &mut ftable::FileTable,
    mounts_fat: &mut Vec<mount::FsMount>,
    mounts: &[mount_legacy::Mount],
    chan: u64,
    flags: u64,
    path: &str,
    fgen: &mut u64,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }

    // Cerca nei mount point registrati (devfs, console, userdisk, futuri driver).
    if let Some((driver_chan, rel)) = mount_legacy::resolve_mount(path, mounts) {
        // Nodo disco raw (Fase 16/16d): open("/dev/sda") o degli alias
        // stabili ("/dev/disk/by-uuid/<H>", "/dev/disk/by-label/<N>") matcha
        // il prefix del nodo stesso (rel vuota) — e cosi' i device registrati
        // per-nome ("/dev/null": rel vuota sul prefix esatto, Fase 16d).
        // L'handle/tipo si chiede al driver (nomi `sdX` via parse locale +
        // validazione, by-path via DISK_RESOLVE, device via dev_type
        // sull'ultimo componente). Impossibile o driver irraggiungibile →
        // None (-1, mai wedge).
        if rel.is_empty() {
            let prefix = path.trim_start_matches('/');
            if let Some(name) = prefix.strip_prefix("dev/") {
                // Alias stabili (by-uuid/by-label): SOLO qui si interroga
                // userdisk (DISK_RESOLVE). Un open di /dev/null NON deve fare
                // un round-trip al disco a ogni chiamata (il flood di t30 lo
                // amplificava a ~1000 HELLO).
                if name.starts_with("disk/by-") {
                    let h = mount::resolve_mount_source(&alloc::format!("/dev/{}", name))?;
                    let reply = libr::send(driver_chan, DEV_OPEN, h as u64, 0).ok()?;
                    if reply.w0 == ERR {
                        return None;
                    }
                    return Some(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
                // Nodo disco raw (/dev/sda, /dev/sdb...): parse locale
                // validato dal driver con DEV_OPEN.
                if let Some(h) = mount_legacy::disk_handle(name) {
                    let reply = libr::send(driver_chan, DEV_OPEN, h as u64, 0).ok()?;
                    if reply.w0 == ERR {
                        return None;
                    }
                    return Some(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
                // Device registrato per-nome (null/zero/...): tipo dall'ultimo
                // componente del prefix esatto.
                if let Some(dev) = name.rsplit('/').next().and_then(mount_legacy::dev_type) {
                    let reply = libr::send(driver_chan, DEV_OPEN, dev, 0).ok()?;
                    if reply.w0 == ERR {
                        return None;
                    }
                    return Some(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
            }
            return None;
        }
        let device_type = mount_legacy::dev_type(rel)?;
        let reply = libr::send(driver_chan, DEV_OPEN, device_type, 0).ok()?;
        let remote_fd = reply.w0 as u32;
        return Some(ftable.open_remote(chan, driver_chan, remote_fd));
    }

    // Filesystem locali: prima i mount FAT (con attivazione lazy), poi ramfs.
    // resolve_local copre ramfs + il caso "mount noto ma inattivo" (→ None,
    // mai shadow in ramfs: stesso contratto di prima).
    if let Some((mi, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        // POSIX come ramfs (20.4): con O_CREAT crea l'entry 8.3 se manca
        // (no LFN, mkdir-su-FAT fuori scope); senza, il file deve esistere.
        // La creazione muta la directory: bumpa la generazione (invalida le
        // cache FileInfo: la nuova entry cambia il layout dir).
        if flags as u32 & libr::O_CREAT != 0 {
            if let Some(fat) = mounts_fat.get(mi).and_then(|m| m.fat()) {
                if fat.find(rel).is_none() {
                    let _ = fat.create_file(rel);
                    *fgen = fgen.wrapping_add(1);
                }
            }
        }
        let fat = mounts_fat[mi].fat()?;
        let info = fat.find(rel)?;
        return Some(ftable.open_fat(chan, rel, mi, info, *fgen));
    }
    match mount_legacy::resolve_local(mounts_fat, path)? {
        mount_legacy::FsKind::Fat => None, // mount inattivo: errore, mai shadow ramfs
        mount_legacy::FsKind::Ram => {
            // POSIX (Fase 18.2, ADR-0015): senza O_CREAT il file deve
            // esistere — mai creare. Con O_CREAT, crea se manca (su dir
            // esistente resta apribile come prima: create_file → None
            // ignorato, poi find).
            if flags as u32 & libr::O_CREAT != 0 {
                let _ = fs.create_file(path);
            }
            fs.find(path)?;
            Some(ftable.open(chan, path, mount_legacy::FsKind::Ram, None))
        }
    }
}

pub fn handle_read(
    fs: &ramfs::RamFs,
    ftable: &mut ftable::FileTable,
    mounts_fat: &mut Vec<mount::FsMount>,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    fd: u32,
    count: usize,
    fgen: &mut u64,
) -> Option<u64> {
    if count > 4096 {
        return None;
    }

    // File remoto: inoltro al driver. userfs inietta entrambi i ring del
    // client nel driver (`map_in`): il driver legge dalla request ring e
    // scrive nella response ring → zero copie (Fase 10.2).
    if let Some((driver_chan, remote_fd)) = ftable.get_remote(chan, fd) {
        let (req_phys, resp_phys) = match rings.get(&chan) {
            Some(&r) => r,
            None => return None,
        };
        libr::map_in(driver_chan, req_phys, libr::CLI_REQ_VA, 1).ok()?;
        libr::map_in(driver_chan, resp_phys, libr::CLI_RESP_VA, 1).ok()?;
        let reply = libr::send(driver_chan, DEV_READ, remote_fd as u64, count as u64).ok()?;
        return Some(reply.w0);
    }
    if ftable.get(chan, fd).is_none() {
        return None;
    }

    let (path, kind, offset, mnt) = ftable.get(chan, fd)?;

    // 24.2 — buffer di risposta sullo stack (count ≤ 4096 per il check in
    // testa): niente `to_vec()`/`Vec` temporanei per-op (la free-list
    // dell'heap di userfs cresceva di ~1 blocco a op FAT → scansioni O(n)
    // su tutte le op successive; vedi read_dir in fat32.rs).
    let mut buf_stack = [0u8; 4096];
    let data: &[u8] = match kind {
        mount_legacy::FsKind::Ram => {
            let d = match fs.find(path)? {
                ramfs::FsNode::File { data: d, .. } => d,
                _ => return None,
            };
            if offset >= d.len() {
                &[]
            } else {
                let end = (offset + count).min(d.len());
                let n = end - offset;
                buf_stack[..n].copy_from_slice(&d[offset..end]);
                &buf_stack[..n]
            }
        }
        mount_legacy::FsKind::Fat => {
            let mi = mnt?;
            // Il mount puo' essere caduto inattivo alla morte di userdisk
            // (drop d'epoca in `note_peer_death`): riattiva per nome qui, come
            // `resolve_fsmount` fa per open/readdir (fail-loud, mai shadow).
            if !mount::reactivate_mount(mounts_fat, mi, fgen) {
                return None;
            }
            let g = *fgen;
            let fat = mounts_fat.get(mi)?.fat()?;
            let info = ftable::fd_fat_info(ftable, fat, chan, fd, g)?;
            let n = fat.read_file(&info, offset, count, &mut buf_stack[..count]);
            &buf_stack[..n]
        }
    };

    let bytes_read = data.len();
    // Frame SEMPRE, anche vuoto a EOF (Fase 18.2-bis): senza, il client non
    // trova risposta e riporta -1 invece di 0. Stesso contratto dei driver
    // (console scrive sempre il frame). Short (< count) o vuoto (0) = EOF.
    if let Some(&(_, _)) = rings.get(&chan) {
        rings::map_client_resp_ring(rings, chan);
        rings::resp_ring_write(bytes_read as u64, 0, &data);
    }
    ftable.set_offset(chan, fd, offset + bytes_read);
    Some(bytes_read as u64)
}

/// File remoto: inoltro al driver. Il payload del WRITE RESTA nel request ring
/// del client (zero copy): userfs inietta entrambi i ring del client nel driver
/// (`map_in`), il driver legge i dati direttamente dal request ring e avanza la
/// tail (SPSC). La chiamata NON deve consumare il frame nel request ring.
/// Ritorna i byte accettati dal driver (reply.w0).
pub fn handle_write_remote(
    ftable: &ftable::FileTable,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    fd: u32,
    count: usize,
) -> Option<u64> {
    let (driver_chan, remote_fd) = match ftable.get_remote(chan, fd) {
        Some(r) => r,
        None => return None,
    };
    let (req_phys, resp_phys) = match rings.get(&chan) {
        Some(&r) => r,
        None => return None,
    };
    libr::map_in(driver_chan, req_phys, libr::CLI_REQ_VA, 1).ok()?;
    libr::map_in(driver_chan, resp_phys, libr::CLI_RESP_VA, 1).ok()?;
    let reply = libr::send(driver_chan, DEV_WRITE, remote_fd as u64, count as u64).ok()?;
    Some(reply.w0)
}

/// Write locale (ramfs): il frame e' gia' stato consumato e il payload e' in
/// Scrive `count` byte di `payload` sul fd (ramfs o FAT-overwrite). FAT32 e'
/// scrivibile dalla Fase 20 (write-through, niente cache): solo overwrite
/// entro la size esistente (20.2) — la crescita/creazione arrivano dopo.
pub fn handle_write_local(
    fs: &mut ramfs::RamFs,
    ftable: &mut ftable::FileTable,
    mounts_fat: &mut Vec<mount::FsMount>,
    chan: u64,
    fd: u32,
    count: usize,
    payload: &[u8],
    fgen: &mut u64,
) -> Option<u64> {
    let (path, kind, offset, mnt) = ftable.get(chan, fd)?;
    if kind == mount_legacy::FsKind::Fat {
        // Scrittura FAT (Fase 20): overwrite + crescita con allocazione
        // (write-through, niente cache FileInfo: la scrittura puo' cambiare
        // size/first_cluster, quindi dopo si bumpa la generazione e si
        // riaggiorna la cache con un find fresco — un find per write, rumore
        // contro le centinaia di round-trip DISK della scrittura stessa).
        let mi = mnt?;
        if !mount::reactivate_mount(mounts_fat, mi, fgen) {
            return None;
        }
        let g = *fgen;
        let fat = mounts_fat.get(mi)?.fat()?;
        let info = ftable::fd_fat_info(ftable, fat, chan, fd, g)?;
        if info.is_dir {
            return None;
        }
        let n = fat.write_grow(&info, offset, &payload[..count.min(payload.len())]);
        *fgen = fgen.wrapping_add(1);
        let g2 = *fgen;
        // Rileggi l'entry dopo la mutazione (size/first_cluster possono aver
        // cambiato valore): la cache resta valida alla nuova generazione.
        let fat = mounts_fat.get(mi)?.fat()?;
        let rel: String = match ftable.files.get(&(chan, fd)) {
            Some(ftable::FileEntry::Local { path, kind: mount_legacy::FsKind::Fat, .. }) => path.clone(),
            _ => return Some(n as u64),
        };
        let fresh = fat.find(&rel);
        ftable.refresh_fat_info(chan, fd, fresh, g2);
        ftable.set_offset(chan, fd, offset + n);
        return Some(n as u64);
    }

    match fs.find_or_create(path)? {
        ramfs::FsNode::File { data: file_data, .. } => {
            if offset + count > file_data.len() {
                file_data.resize(offset + count, 0);
            }
            file_data[offset..offset + count].copy_from_slice(&payload[..count]);
            ftable.set_offset(chan, fd, offset + count);
            Some(count as u64)
        }
        _ => None,
    }
}

pub fn handle_close(ftable: &mut ftable::FileTable, chan: u64, fd: u32) -> Option<u64> {
    // File remoto: chiudi anche sul server.
    if let Some((driver_chan, remote_fd)) = ftable.get_remote(chan, fd) {
        let _ = libr::send(driver_chan, DEV_CLOSE, remote_fd as u64, 0);
    }
    if ftable.close(chan, fd) { Some(0) } else { None }
}

pub fn handle_readdir(
    fs: &ramfs::RamFs,
    mounts_fat: &mut Vec<mount::FsMount>,
    mounts: &[mount_legacy::Mount],
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    path: &str,
    fgen: &mut u64,
) -> Option<u64> {
    // Directory remota (device): inoltro al driver, che scrive le entry nella
    // response ring del client (mappata li' da map_in).
    if let Some((driver_chan, _rel)) = mount_legacy::resolve_mount(path, mounts) {
        let (req_phys, resp_phys) = rings.get(&chan)?;
        libr::map_in(driver_chan, *req_phys, libr::CLI_REQ_VA, 1).ok()?;
        libr::map_in(driver_chan, *resp_phys, libr::CLI_RESP_VA, 1).ok()?;
        let reply = libr::send(driver_chan, DEV_READDIR, 0, 0).ok()?;
        return Some(reply.w0);
    }

    if let Some((mi, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        let fat = mounts_fat[mi].fat()?;
        let entries: Vec<String> =
            fat.list_dir(rel).into_iter().map(|d| d.name).collect();
        // Mount annidati sotto dir FAT (edge raro, gratis col design union).
        let entries = mount_legacy::union_mount_children(entries, mounts, mounts_fat, path);
        let mut buf = Vec::new();
        for entry in &entries {
            buf.extend_from_slice(entry.as_bytes());
            buf.push(0);
        }
        buf.push(0);
        if let Some(&(_, _)) = rings.get(&chan) {
            rings::map_client_resp_ring(rings, chan);
            rings::resp_ring_write(entries.len() as u64, 0, &buf);
        }
        return Some(entries.len() as u64);
    }

    // Listing sintetizzato dai prefix registrati (Fase 16d, discovery):
    // se `path` e' directory padre di prefix noti (es. "/dev",
    // "/dev/disk/by-uuid") elenca i figli immediati. Solo dove ramfs/fat non
    // hanno la dir (mai shadow, mai cambi ai listing esistenti). Nota:
    // `resolve_local` esclude i path /dev/* (None) prima ancora di guardare
    // ramfs — la sintesi copre anche quelli.
    //
    // Fase 18.1-ter: i mount point si mergiano SEMPRE (union con dedupe, mai
    // shadow): `ls /` mostra ramfs + `fat` + `dev`. Solo nomi, mai contenuti:
    // il check subtree Fase 17 sul path richiesto resta prima del dispatch.
    // Directory esistente ma vuota resta OK (exists): solo "sconosciuto E
    // senza mount" e' errore.
    let (exists, base): (bool, Vec<String>) = match mount_legacy::resolve_local(mounts_fat, path) {
        Some(mount_legacy::FsKind::Ram) => match fs.readdir(path) {
            Some(e) => (true, e),
            None => (false, Vec::new()),
        },
        Some(mount_legacy::FsKind::Fat) => return None, // mount noto ma inattivo: errore
        None => (false, Vec::new()),
    };
    let entries = mount_legacy::union_mount_children(base, mounts, mounts_fat, path);
    if !exists && entries.is_empty() {
        return None;
    }

    let mut buf = Vec::new();
    for entry in &entries {
        buf.extend_from_slice(entry.as_bytes());
        buf.push(0);
    }
    buf.push(0);
    // Scrivi le entry nella response ring del client.
    if let Some(&(_, _)) = rings.get(&chan) {
        rings::map_client_resp_ring(rings, chan);
        rings::resp_ring_write(entries.len() as u64, 0, &buf);
    }
    Some(entries.len() as u64)
}

/// Scrive il response frame di R_STAT (`[size:8][kind:8]`, payload vuoto) e
/// ritorna 0 per il reply IPC (self-written: il dispatch non riscrive).
pub fn stat_reply(rings: &BTreeMap<u64, (u64, u64)>, chan: u64, size: u64, kind: u64) -> u64 {
    if rings.get(&chan).is_some() {
        rings::map_client_resp_ring(rings, chan);
        rings::resp_ring_write(size, kind, &[]);
    }
    0
}

/// R_STAT: metadati del path (Fase 19.2, zero kernel). Self-written come
/// read/readdir (frame `[size:8][kind:8]`, vedi `stat_reply`); None =
/// inesistente. Precedenza come open (mai shadow): device esatti → FAT (con
/// attivazione lazy) → ramfs → padri sintetizzati 16d → None. Mount FAT noto
/// ma inattivo = errore (stesso contratto di open/readdir). kind in
/// `syscall-numbers` (STAT_FILE/DIR/DEVICE + STAT_READONLY): ramfs da' len
/// reale (dir = 0, mai readonly), FAT size dalla dir entry (scrivibile dalla
/// Fase 20: mai readonly), device size 0 readonly 0 (sconosciuto senza
/// interrogare il driver: i prefix registrati sono foglie, qui mai contattati).
pub fn handle_stat(
    fs: &ramfs::RamFs,
    mounts_fat: &mut Vec<mount::FsMount>,
    mounts: &[mount_legacy::Mount],
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    path: &str,
    fgen: &mut u64,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // Root ramfs: esiste sempre.
    if path == "/" {
        return Some(stat_reply(rings, chan, 0, libr::STAT_DIR));
    }
    // Device registrati: foglie (rel non vuota = path sotto un device: None,
    // come open che rifiuta i dev_type sconosciuti).
    if let Some((_driver_chan, rel)) = mount_legacy::resolve_mount(path, mounts) {
        if rel.is_empty() {
            return Some(stat_reply(rings, chan, 0, libr::STAT_DEVICE));
        }
        return None;
    }
    // FAT con attivazione lazy; mount noto ma inattivo = errore, mai shadow.
    // (find fresco a ogni stat: niente fd, niente cache — i metadati non
    // devono mai essere stale.)
    if let Some((mi, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        let fat = mounts_fat[mi].fat()?;
        if rel.is_empty() {
            return Some(stat_reply(rings, chan, 0, libr::STAT_DIR));
        }
        let info = fat.find(rel)?;
        let kind = if info.is_dir { libr::STAT_DIR } else { libr::STAT_FILE };
        return Some(stat_reply(rings, chan, info.size as u64, kind));
    }
    match mount_legacy::resolve_local(mounts_fat, path) {
        // Mount noto ma inattivo: errore, mai shadow ramfs.
        Some(mount_legacy::FsKind::Fat) => None,
        Some(mount_legacy::FsKind::Ram) => match fs.find(path) {
            Some(ramfs::FsNode::File { data, .. }) => {
                Some(stat_reply(rings, chan, data.len() as u64, libr::STAT_FILE))
            }
            Some(ramfs::FsNode::Dir { .. }) => {
                Some(stat_reply(rings, chan, 0, libr::STAT_DIR))
            }
            // Non in ramfs: puo' essere un padre sintetizzato (sotto).
            None => mount_legacy::synth_children(mounts, path)
                .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR)),
        },
        // /dev/* senza prefix noto: solo sintesi (sotto).
        None => mount_legacy::synth_children(mounts, path)
            .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR)),
    }
}

pub fn handle_mkdir(fs: &mut ramfs::RamFs, mounts: &[mount::FsMount], path: &str) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // mkdir solo su ramfs (i mount FAT/remoti non hanno mkdir).
    match mount_legacy::resolve_local(mounts, path)? {
        mount_legacy::FsKind::Ram => {
            fs.mkdir(path)?;
            Some(0)
        }
        _ => None,
    }
}

/// Cancella un file o una directory VUOTA (Fase 18.2, `R_DELETE`).
/// Solo ramfs: su FAT manca l'unlink (e' scrivibile dalla Fase 20, ma non
/// cancellabile) e i device remoti non sono file cancellabili
/// (e un mount point non si rimuove: si smonta). Ritorna Some(0) o None.
pub fn handle_delete(
    fs: &mut ramfs::RamFs,
    mounts_fat: &[mount::FsMount],
    mounts: &[mount_legacy::Mount],
    path: &str,
) -> Option<u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    // Mai dentro driver remoti…
    if mount_legacy::resolve_mount(path, mounts).is_some() {
        return None;
    }
    // …e mai su mount FAT (unlink non implementato): solo ramfs.
    match mount_legacy::resolve_local(mounts_fat, path)? {
        mount_legacy::FsKind::Ram => {
            fs.remove(path)?;
            Some(0)
        }
        _ => None,
    }
}

/// Monta una sorgente sul target (Fase 16b, payload "source\0target\0").
/// Ritorna Some(0) se il mount e' ATTIVO, None altrimenti: a resolve fallito
/// (sorgente/target invalidi, nome ignoto, driver irraggiungibile) nessun
/// cambio di stato; a BPB illeggibile la spec resta registrata INATTIVA e
/// ritenta lazy (mai shadow ramfs).
pub fn handle_mount(mounts: &mut Vec<mount::FsMount>, payload: &str, fgen: &mut u64) -> Option<u64> {
    let mut parts = payload.split('\0');
    let source = parts.next()?;
    let target = parts.next()?;
    if source.is_empty() || target.is_empty() {
        return None;
    }
    if mount::apply_mount_spec(mounts, source, target, "") {
        // La tabella e' cambiata (spec nuova/sostituita): invalida le cache.
        *fgen = fgen.wrapping_add(1);
        Some(0)
    } else {
        None
    }
}

/// Smonta un target (Fase 16b). Rifiutato se ci sono fd aperti sotto il mount
/// (EBUSY); la radice ramfs non e' smontabile (non e' in tabella).
/// A rimozione riuscita bumpa `gen` (gli indici mount degli fd restanti non
/// cambiano per EBUSY, ma le istanze vanno comunque ricontrollate).
pub fn handle_umount(
    mounts: &mut Vec<mount::FsMount>,
    ftable: &ftable::FileTable,
    target: &str,
    fgen: &mut u64,
) -> Option<u64> {
    let norm = mount::normalize_target(target)?;
    let idx = mounts.iter().position(|m| m.target == norm)?;
    if ftable.has_mount_users(idx) {
        return None;
    }
    mounts.remove(idx);
    *fgen = fgen.wrapping_add(1);
    Some(0)
}
