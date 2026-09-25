use super::*;
use crate::provider::LocalFs;

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
) -> Result<u64, u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(ERR_INVALID);
    }
    let flags = flags as u32;
    let creat = flags & libr::O_CREAT != 0;
    let trunc = flags & libr::O_TRUNC != 0;
    let append = flags & libr::O_APPEND != 0;

    // Cerca nei mount point registrati (devfs, console, userdisk, futuri driver).
    if let Some((driver_chan, rel)) = mount_legacy::resolve_mount(path, mounts) {
        // (Ramo device invariato: gli errori dei driver restano opachi —
        // nessun dominio attribuibile senza interrogarli.)
        if rel.is_empty() {
            let prefix = path.trim_start_matches('/');
            if let Some(name) = prefix.strip_prefix("dev/") {
                if name.starts_with("disk/by-") {
                    let h = mount::resolve_mount_source(&alloc::format!("/dev/{}", name)).ok_or(ERR)?;
                    let reply = libr::send(driver_chan, DEV_OPEN, h as u64, 0).map_err(|_| ERR)?;
                    if reply.w0 == ERR {
                        return Err(ERR);
                    }
                    return Ok(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
                if let Some(h) = mount_legacy::disk_handle(name) {
                    let reply = libr::send(driver_chan, DEV_OPEN, h as u64, 0).map_err(|_| ERR)?;
                    if reply.w0 == ERR {
                        return Err(ERR);
                    }
                    return Ok(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
                if let Some(dev) = name.rsplit('/').next().and_then(mount_legacy::dev_type) {
                    let reply = libr::send(driver_chan, DEV_OPEN, dev, 0).map_err(|_| ERR)?;
                    if reply.w0 == ERR {
                        return Err(ERR);
                    }
                    return Ok(ftable.open_remote(chan, driver_chan, reply.w0 as u32));
                }
            }
            return Err(ERR_NOTFOUND);
        }
        let device_type = mount_legacy::dev_type(rel).ok_or(ERR_NOTFOUND)?;
        let reply = libr::send(driver_chan, DEV_OPEN, device_type, 0).map_err(|_| ERR)?;
        let remote_fd = reply.w0 as u32;
        return Ok(ftable.open_remote(chan, driver_chan, remote_fd));
    }

    // Filesystem locali: prima i mount (FAT con attivazione lazy, Local
    // sempre attivi), poi ramfs radice.
    if let Some((mid, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        // Mount `Local` (Fase 49, F4): open via `open_dyn`, handle AnyHandle
        // nell'fd (mai reopen per-path, mai cache da invalidare). Sonda senza
        // tenere borrow oltre l'op (le varianti sono esclusive).
        let is_local = matches!(
            mount::by_id(mounts_fat, mid).and_then(|i| mounts_fat.get(i)),
            Some(m) if m.is_local()
        );
        if is_local {
            let h = mount::by_id_mut(mounts_fat, mid)
                .ok_or(ERR)?
                .local_dyn()
                .ok_or(ERR)?
                .open_dyn(rel, flags)
                .map_err(|_| ERR_NOTFOUND)?;
            return Ok(ftable.open_local(chan, rel, mid, h, append));
        }
        // 49.5 — open FAT via trait `LocalFs` sul concreto (F5): `Fat32::open`
        // assorbe O_CREAT (crea), O_TRUNC (tronca) e rifiuta le dir (ISDIR)
        // come `RamFs::open` — niente piu' create_file/truncate/find fuori
        // trait. La cache per-fd resta in ftable (path-based storage, come U1).
        let info = mount::by_id_mut(mounts_fat, mid)
            .ok_or(ERR)?
            .fat_mut()
            .ok_or(ERR)?
            .open(rel, flags)
            .map_err(|_| ERR_NOTFOUND)?;
        // O_CREAT/O_TRUNC possono aver mutato il volume: invalida le cache.
        if creat || trunc {
            *fgen = fgen.wrapping_add(1);
        }
        return Ok(ftable.open_fat(chan, rel, mid, info, *fgen, append));
    }
    match mount_legacy::resolve_local(mounts_fat, path).ok_or(ERR_NOTFOUND)? {
        // Mount noto ma inattivo, o Local non risolto sopra (difensivo: i
        // Local attivi passano sempre da resolve_fsmount): errore, mai shadow.
        mount_legacy::FsKind::Fat | mount_legacy::FsKind::Local => Err(ERR),
        mount_legacy::FsKind::Ram => {
            // 47.1 — open via trait `LocalFs` (U1): la trait gestisce O_CREAT,
            // validazione file/dir e O_TRUNC internamente; il RamHandle restituito
            // non si memorizza in ftable (U1 mantiene path-based storage).
            let _ = crate::provider::LocalFs::open(fs, path, flags as u32)?;
            Ok(ftable.open(chan, path, mount_legacy::FsKind::Ram, None, append))
        }
    }
}

pub fn handle_read(
    fs: &mut ramfs::RamFs,
    ftable: &mut ftable::FileTable,
    pipes: &mut pipes::PipeTable,
    mounts_fat: &mut Vec<mount::FsMount>,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    fd: u32,
    count: usize,
    fgen: &mut u64,
) -> Result<u64, u64> {
    if count > 4096 {
        return Err(ERR_INVALID);
    }

    // File remoto: inoltro al driver. userfs inietta entrambi i ring del
    // client nel driver (`map_in`): il driver legge dalla request ring e
    // scrive nella response ring → zero copie (Fase 10.2).
    if let Some((driver_chan, remote_fd)) = ftable.get_remote(chan, fd) {
        let (req_phys, resp_phys) = match rings.get(&chan) {
            Some(&r) => r,
            None => return Err(ERR),
        };
        libr::map_in(driver_chan, req_phys, libr::CLI_REQ_VA, 1).map_err(|_| ERR)?;
        libr::map_in(driver_chan, resp_phys, libr::CLI_RESP_VA, 1).map_err(|_| ERR)?;
        let reply = libr::send(driver_chan, DEV_READ, remote_fd as u64, count as u64).map_err(|_| ERR)?;
        return Ok(reply.w0);
    }
    // Estremita' di pipe in lettura (Fase 42): mai dalla tail condivisa, mai
    // blocco — vedi handle_pipe_read (Empty vs EOF distinti per costruzione).
    if let Some((pipe_id, write)) = ftable.get_pipe(chan, fd) {
        if write {
            return Err(ERR_INVALID); // read sul lato scrittura
        }
        return handle_pipe_read(pipes, rings, chan, pipe_id, count);
    }
    if ftable.get(chan, fd).is_none() {
        return Err(ERR_INVALID);
    }

    let (path, kind, offset, mnt) = ftable.get(chan, fd).ok_or(ERR_INVALID)?;

    // 24.2 — buffer di risposta sullo stack (count ≤ 4096 per il check in
    // testa): niente `to_vec()`/`Vec` temporanei per-op (la free-list
    // dell'heap di userfs cresceva di ~1 blocco a op FAT → scansioni O(n)
    // su tutte le op successive; vedi read_dir in fat32.rs).
    let mut buf_stack = [0u8; 4096];
    let data: &[u8] = match kind {
        mount_legacy::FsKind::Ram => {
            // 47.2 — read via trait `LocalFs` (U1): open per ottenere handle,
            // poi read attraverso la trait. Flags=0 = sola lettura (no create/truncate).
            let handle = crate::provider::LocalFs::open(fs, path, 0)?;
            let n = crate::provider::LocalFs::read(fs, handle, offset, &mut buf_stack[..count])?;
            &buf_stack[..n]
        }
        mount_legacy::FsKind::Local => {
            // 49.4 — read via `AnyHandle` dell'fd (Fase 49, F4): aperto una
            // volta con `open_dyn`, mai reopen per-path.
            let h = ftable.get_dyn_handle(chan, fd).ok_or(ERR_INVALID)?;
            let mid = mnt.ok_or(ERR)?;
            let d = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
            let n = d.read_dyn(h, offset, &mut buf_stack[..count])?;
            &buf_stack[..n]
        }
        mount_legacy::FsKind::Fat => {
            // 49.1 — read via trait `LocalFsDyn` con `AnyHandle` by-value
            // (Fase 49, F1): niente piu' `*const ()`. La cache FileInfo per-fd
            // (Fase 21) resta la sorgente dell'handle: un reopen per path
            // farebbe un find a OGNI read (~8x sui load da disco, regressione
            // misurata nei restart t27/t28/t32). L'handle e' copiato sullo
            // stack (FileInfo: Copy, niente heap per-op, regola Fase 24).
            let mid = mnt.ok_or(ERR)?;
            // Il mount puo' essere caduto inattivo alla morte di userdisk
            // (drop d'epoca in `note_peer_death`): riattiva per id qui, come
            // `resolve_fsmount` fa per open/readdir (fail-loud, mai shadow).
            if !mount::reactivate_mount_by_id(mounts_fat, mid, fgen) {
                return Err(ERR);
            }
            let g = *fgen;
            let fat_c = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.fat().ok_or(ERR)?;
            let info = ftable::fd_fat_info(ftable, fat_c, chan, fd, g).ok_or(ERR_NOTFOUND)?;
            if info.is_dir {
                return Err(ERR_ISDIR);
            }
            let fat = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
            let n = crate::provider::LocalFsDyn::read_dyn(
                fat,
                crate::provider::AnyHandle::Fat(info),
                offset,
                &mut buf_stack[..count],
            )?;
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
    Ok(bytes_read as u64)
}

/// Read da estremita' pipe (Fase 42): self-written come le read locali
/// (il dispatch non riscrive per R_READ). Tre esiti, mai blocco:
/// dati → frame con payload; vuota con writer aperti → ERR_EMPTY (il client
/// riprova throttled: 0 significherebbe EOF e troncherebbe la pipeline);
/// vuota con writer chiusi → frame vuoto + 0 (EOF vero, contratto 18.2-bis).
fn handle_pipe_read(
    pipes: &mut pipes::PipeTable,
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    pipe_id: u32,
    count: usize,
) -> Result<u64, u64> {
    let mut buf_stack = [0u8; 4096];
    let n = count.min(4096);
    let (got, eof) = match pipes.read(pipe_id, &mut buf_stack[..n]) {
        Some(v) => v,
        None => return Err(ERR), // id ignoto: inconsistenza interna
    };
    if got == 0 && !eof {
        if rings.get(&chan).is_some() {
            rings::map_client_resp_ring(rings, chan);
            rings::resp_ring_write(ERR_EMPTY, 0, &[]);
        }
        return Err(ERR_EMPTY);
    }
    if let Some(&(_, _)) = rings.get(&chan) {
        rings::map_client_resp_ring(rings, chan);
        rings::resp_ring_write(got as u64, 0, &buf_stack[..got]);
    }
    Ok(got as u64)
}

/// Crea una pipe (Fase 42): buffer + due fd (lettura, scrittura) sul canale
/// del chiamante. Ritorna (read_fd, write_fd): il dispatch li mette in
/// result e w1 del response frame.
pub fn handle_pipe_create(
    ftable: &mut ftable::FileTable,
    pipes: &mut pipes::PipeTable,
    chan: u64,
    hint: usize,
) -> Result<(u64, u64), u64> {
    let id = pipes.create(hint);
    let r = ftable.open_pipe(chan, id, false);
    let w = ftable.open_pipe(chan, id, true);
    Ok((r, w))
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
) -> Result<u64, u64> {
    let (driver_chan, remote_fd) = match ftable.get_remote(chan, fd) {
        Some(r) => r,
        None => return Err(ERR_INVALID),
    };
    let (req_phys, resp_phys) = match rings.get(&chan) {
        Some(&r) => r,
        None => return Err(ERR),
    };
    libr::map_in(driver_chan, req_phys, libr::CLI_REQ_VA, 1).map_err(|_| ERR)?;
    libr::map_in(driver_chan, resp_phys, libr::CLI_RESP_VA, 1).map_err(|_| ERR)?;
    let reply = libr::send(driver_chan, DEV_WRITE, remote_fd as u64, count as u64).map_err(|_| ERR)?;
    Ok(reply.w0)
}

/// Write locale (ramfs): il frame e' gia' stato consumato e il payload e' in
/// Scrive `count` byte di `payload` sul fd (ramfs o FAT-overwrite). FAT32 e'
/// scrivibile dalla Fase 20 (write-through, niente cache): solo overwrite
/// entro la size esistente (20.2) — la crescita/creazione arrivano dopo.
pub fn handle_write_local(
    fs: &mut ramfs::RamFs,
    ftable: &mut ftable::FileTable,
    pipes: &mut pipes::PipeTable,
    mounts_fat: &mut Vec<mount::FsMount>,
    chan: u64,
    fd: u32,
    count: usize,
    payload: &[u8],
    fgen: &mut u64,
) -> Result<u64, u64> {
    // Estremita' di pipe in scrittura (Fase 42): append/offset ignorati (le
    // pipe non hanno offset); oltre la capacita' = parziale (il client
    // rimanda); senza lettori = ERR_CLOSED (SIGPIPE senza segnali).
    if let Some((pipe_id, write)) = ftable.get_pipe(chan, fd) {
        if !write {
            return Err(ERR_INVALID); // write sul lato lettura
        }
        if !pipes.has_readers(pipe_id) {
            return Err(ERR_CLOSED);
        }
        let want = count.min(payload.len());
        match pipes.write(pipe_id, &payload[..want]) {
            // Zero accettati a lettori vivi (piena): NON 0 (il client lo
            // leggerebbe come "fatto") ma ERR_EMPTY — il client riprova
            // throttled finche' il lettore drena (wrapping di write_fs).
            Some(0) => return Err(ERR_EMPTY),
            Some(n) => return Ok(n as u64),
            None => return Err(ERR),
        }
    }
    let (path, kind, offset, mnt) = ftable.get(chan, fd).ok_or(ERR_INVALID)?;
    let append = ftable.is_append(chan, fd);
    if kind == mount_legacy::FsKind::Local {
        // Scrittura su mount `Local` (Fase 49, F4): handle dell'fd, mai
        // reopen; niente generazioni (il provider e' authoritative).
        let h = ftable.get_dyn_handle(chan, fd).ok_or(ERR_INVALID)?;
        let mid = mnt.ok_or(ERR)?;
        let d = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
        let n = d.write_dyn(h, offset, &payload[..count.min(payload.len())], append)?;
        // O_APPEND non usa `offset`: il nuovo offset e' la size dopo la
        // scrittura (via stat fresca, mai stale oltre l'op).
        let new_off = if append {
            let rel_owned: String = alloc::string::String::from(path);
            let d = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
            d.stat_dyn(&rel_owned).map(|m| m.size as usize).unwrap_or(offset + n as usize)
        } else {
            offset + n as usize
        };
        ftable.set_offset(chan, fd, new_off);
        return Ok(n as u64);
    }
    if kind == mount_legacy::FsKind::Fat {
        // Scrittura FAT (Fase 20): overwrite + crescita con allocazione
        // (write-through, niente cache FileInfo: la scrittura puo' cambiare
        // size/first_cluster, quindi dopo si bumpa la generazione e si
        // riaggiorna la cache con un find fresco — un find per write, rumore
        // contro le centinaia di round-trip DISK della scrittura stessa).
        let mi = mnt.ok_or(ERR)?;
        if !mount::reactivate_mount_by_id(mounts_fat, mi, fgen) {
            return Err(ERR);
        }
        let g = *fgen;
        // `path` presta da ftable: clonato una volta (come il vecchio handler)
        // per poter prendere ftable in mut per la cache FileInfo.
        let rel_path: String = alloc::string::String::from(path);
        // Cache FileInfo per-fd come la read (evita un find per write). L'handle
        // passato alla trait e' il FileInfo cachato (copia sullo stack, F1).
        let fat_c = mount::by_id_mut(mounts_fat, mi).ok_or(ERR)?.fat().ok_or(ERR)?;
        let info = ftable::fd_fat_info(ftable, fat_c, chan, fd, g).ok_or(ERR_NOTFOUND)?;
        if info.is_dir {
            return Err(ERR_ISDIR);
        }
        let fat = mount::by_id_mut(mounts_fat, mi).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
        let n = crate::provider::LocalFsDyn::write_dyn(
            fat,
            crate::provider::AnyHandle::Fat(info),
            offset,
            &payload[..count.min(payload.len())],
            append,
        )?;
        *fgen = fgen.wrapping_add(1);
        let g2 = *fgen;
        // Rileggi l'entry dopo la mutazione (size/first_cluster possono aver
        // cambiato valore): la cache resta valida alla nuova generazione.
        let fat_c = mount::by_id_mut(mounts_fat, mi).ok_or(ERR)?.fat().ok_or(ERR)?;
        let fresh = fat_c.find(&rel_path);
        // O_APPEND non usa `offset` del fd: il nuovo offset e' la size dopo la
        // scrittura. Altrimenti offset + n (contratto ramfs).
        let new_off = if append {
            fresh.map(|i| i.size as usize).unwrap_or(offset + n as usize)
        } else {
            offset + n as usize
        };
        ftable.refresh_fat_info(chan, fd, fresh, g2);
        ftable.set_offset(chan, fd, new_off);
        return Ok(n as u64);
    }

    // 47.3 — write via trait `LocalFs` (U1): open con O_CREAT per creare file
    // inesistenti, poi write attraverso la trait (gestisce resize + copy).
    let handle = crate::provider::LocalFs::open(fs, path, libr::O_CREAT)?;
    let n = crate::provider::LocalFs::write(fs, handle, offset, payload, append)?;
    ftable.set_offset(chan, fd, offset + n as usize);
    Ok(n as u64)
}

pub fn handle_close(
    ftable: &mut ftable::FileTable,
    pipes: &mut pipes::PipeTable,
    chan: u64,
    fd: u32,
) -> Result<u64, u64> {
    // Estremita' pipe: rimuovi l'fd e decrementa il conteggio (l'ultima
    // close libera il buffer, mai leak a pipeline finite).
    if let Some((pipe_id, write)) = ftable.get_pipe(chan, fd) {
        ftable.close(chan, fd);
        pipes.end_closed(pipe_id, write);
        return Ok(0);
    }
    // File remoto: chiudi anche sul server.
    if let Some((driver_chan, remote_fd)) = ftable.get_remote(chan, fd) {
        let _ = libr::send(driver_chan, DEV_CLOSE, remote_fd as u64, 0);
    }
    if ftable.close(chan, fd) { Ok(0) } else { Err(ERR_INVALID) }
}

pub fn handle_readdir(
    fs: &mut ramfs::RamFs,
    mounts_fat: &mut Vec<mount::FsMount>,
    mounts: &[mount_legacy::Mount],
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    path: &str,
    fgen: &mut u64,
) -> Result<u64, u64> {
    // Directory remota (device): inoltro al driver, che scrive le entry nella
    // response ring del client (mappata li' da map_in).
    if let Some((driver_chan, _rel)) = mount_legacy::resolve_mount(path, mounts) {
        let (req_phys, resp_phys) = rings.get(&chan).ok_or(ERR)?;
        libr::map_in(driver_chan, *req_phys, libr::CLI_REQ_VA, 1).map_err(|_| ERR)?;
        libr::map_in(driver_chan, *resp_phys, libr::CLI_RESP_VA, 1).map_err(|_| ERR)?;
        let reply = libr::send(driver_chan, DEV_READDIR, 0, 0).map_err(|_| ERR)?;
        return Ok(reply.w0);
    }

    if let Some((mid, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        // 48.5 — readdir via trait `LocalFsDyn`: la trait gestisce il path relativo al mount.
        let mut entries: Vec<String> = Vec::new();
        {
            let fat = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
            struct CollectSink<'a>(&'a mut Vec<String>);
            impl crate::provider::EntrySink for CollectSink<'_> {
                fn emit(&mut self, name: &str) {
                    self.0.push(alloc::string::String::from(name));
                }
            }
            let _ = crate::provider::LocalFsDyn::readdir_dyn(fat, rel, &mut CollectSink(&mut entries))?;
        }
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
        return Ok(entries.len() as u64);
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
        Some(mount_legacy::FsKind::Ram) => {
            // 47.4 — readdir via trait `LocalFs` (U1): sink inline per raccogliere entry.
            // Ok(0) su dir vuota = esiste ma senza figli; Err = inesistente.
            let mut entries: Vec<String> = Vec::new();
            struct CollectSink<'a>(&'a mut Vec<String>);
            impl crate::provider::EntrySink for CollectSink<'_> {
                fn emit(&mut self, name: &str) { self.0.push(alloc::string::String::from(name)); }
            }
            let ok = crate::provider::LocalFs::readdir(fs, path, &mut CollectSink(&mut entries)).is_ok();
            (ok, entries)
        }
        // Mount noto ma inattivo, o Local non risolto sopra (difensivo: i
        // Local attivi passano sempre da resolve_fsmount): errore, mai shadow.
        Some(mount_legacy::FsKind::Fat) | Some(mount_legacy::FsKind::Local) => {
            return Err(ERR_NOTFOUND)
        }
        None => (false, Vec::new()),
    };
    let entries = mount_legacy::union_mount_children(base, mounts, mounts_fat, path);
    if !exists && entries.is_empty() {
        // Distingue "e' un file" (NotDir) da "non esiste" (NotFound): la
        // union sopra e' invariata (mai shadow), si raffina solo l'errore.
        match fs.find(path) {
            Some(ramfs::FsNode::File { .. }) => return Err(ERR_NOTDIR),
            _ => return Err(ERR_NOTFOUND),
        }
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
    Ok(entries.len() as u64)
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

/// Codifica il campo `kind` di R_STAT da `Meta` (Fase 48): bit
/// STAT_FILE/DIR + STAT_READONLY dal provider. `libr::stat` decodifica
/// `w1 & STAT_READONLY`; senza questa propagazione `Meta.readonly` sarebbe
/// ignorato (era il caso prima del wiring FAT).
fn stat_kind(meta: &crate::provider::Meta) -> u64 {
    let base = if meta.kind == 1 { libr::STAT_DIR } else { libr::STAT_FILE };
    if meta.readonly {
        base | libr::STAT_READONLY
    } else {
        base
    }
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
    fs: &mut ramfs::RamFs,
    mounts_fat: &mut Vec<mount::FsMount>,
    mounts: &[mount_legacy::Mount],
    rings: &BTreeMap<u64, (u64, u64)>,
    chan: u64,
    path: &str,
    fgen: &mut u64,
) -> Result<u64, u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(ERR_INVALID);
    }
    // Root ramfs: esiste sempre.
    if path == "/" {
        return Ok(stat_reply(rings, chan, 0, libr::STAT_DIR));
    }
    // Device registrati: foglie (rel non vuota = path sotto un device: None,
    // come open che rifiuta i dev_type sconosciuti).
    if let Some((_driver_chan, rel)) = mount_legacy::resolve_mount(path, mounts) {
        if rel.is_empty() {
            return Ok(stat_reply(rings, chan, 0, libr::STAT_DEVICE));
        }
        return Err(ERR_NOTFOUND);
    }
    // FAT con attivazione lazy; mount noto ma inattivo = errore, mai shadow.
    // (find fresco a ogni stat: niente fd, niente cache — i metadati non
    // devono mai essere stale.)
    if let Some((mid, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        // 48.5 — stat via trait `LocalFsDyn`: la trait gestisce il path relativo al mount.
        let fat = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
        if rel.is_empty() {
            return Ok(stat_reply(rings, chan, 0, libr::STAT_DIR));
        }
        let meta = fat.stat_dyn(rel)?;
        return Ok(stat_reply(rings, chan, meta.size, stat_kind(&meta)));
    }
    match mount_legacy::resolve_local(mounts_fat, path) {
        // Mount noto ma inattivo, o Local non risolto sopra (difensivo):
        // errore, mai shadow ramfs.
        Some(mount_legacy::FsKind::Fat) | Some(mount_legacy::FsKind::Local) => Err(ERR),
        Some(mount_legacy::FsKind::Ram) => {
            // 47.4 — stat via trait `LocalFs` (U1): metadati diretti dalla trait.
            let meta = crate::provider::LocalFs::stat(fs, path)?;
            Ok(stat_reply(rings, chan, meta.size, stat_kind(&meta)))
        }
        // /dev/* senza prefix noto: solo sintesi (sotto).
        None => mount_legacy::synth_children(mounts, path)
            .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR))
            .ok_or(ERR_NOTFOUND),
    }
}

pub fn handle_mkdir(
    fs: &mut ramfs::RamFs,
    mounts_fat: &mut Vec<mount::FsMount>,
    path: &str,
    fgen: &mut u64,
) -> Result<u64, u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(ERR_INVALID);
    }
    // Mount `Local` (Fase 49, F4): mkdir via trait sul mount.
    if let Some((mid, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        let m = mount::by_id_mut(mounts_fat, mid).ok_or(ERR_NOTFOUND)?;
        if m.is_local() {
            let d = m.local_dyn().ok_or(ERR)?;
            d.mkdir_dyn(rel)?;
            return Ok(0);
        }
        return Err(ERR); // FAT: niente mkdir.
    }
    // mkdir solo su ramfs radice (i mount FAT/remoti non hanno mkdir).
    match mount_legacy::resolve_local(mounts_fat, path).ok_or(ERR_NOTFOUND)? {
        mount_legacy::FsKind::Ram => {
            // 47.5 — mkdir via trait `LocalFs` (U1): la trait gestisce Exists vs NotFound.
            crate::provider::LocalFs::mkdir(fs, path)?;
            Ok(0)
        }
        _ => Err(ERR),
    }
}

/// Cancella un file o una directory VUOTA (Fase 18.2, `R_DELETE`).
/// Ramfs radice e mount `Local` (via trait); su FAT manca l'unlink (e'
/// scrivibile dalla Fase 20, ma non cancellabile) e i device remoti non sono
/// file cancellabili (e un mount point non si rimuove: si smonta).
/// Ritorna Some(0) o None.
pub fn handle_delete(
    fs: &mut ramfs::RamFs,
    mounts_fat: &mut Vec<mount::FsMount>,
    mounts: &[mount_legacy::Mount],
    path: &str,
    fgen: &mut u64,
) -> Result<u64, u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(ERR_INVALID);
    }
    // Mai dentro driver remoti…
    if mount_legacy::resolve_mount(path, mounts).is_some() {
        return Err(ERR_INVALID);
    }
    // …mount `Local` via trait (Fase 49, F4)…
    if let Some((mid, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        let m = mount::by_id_mut(mounts_fat, mid).ok_or(ERR_NOTFOUND)?;
        if m.is_local() {
            let d = m.local_dyn().ok_or(ERR)?;
            d.remove_dyn(rel)?;
            return Ok(0);
        }
        return Err(ERR_READONLY); // FAT: unlink non implementato.
    }
    // …e mai su mount FAT inattivo: solo ramfs radice.
    match mount_legacy::resolve_local(mounts_fat, path).ok_or(ERR_NOTFOUND)? {
        mount_legacy::FsKind::Ram => {
            // 47.5 — delete via trait `LocalFs` (U1): la trait gestisce NotFound vs EmptyDir.
            crate::provider::LocalFs::remove(fs, path)?;
            Ok(0)
        }
        _ => Err(ERR_READONLY),
    }
}

/// Monta una sorgente sul target (Fase 16b, payload "source\0target\0").
/// Ritorna Ok(0) se il mount e' ATTIVO, Err tipizzato altrimenti: a resolve
/// fallito (sorgente/target invalidi, nome ignoto, driver irraggiungibile)
/// nessun cambio di stato; a BPB illeggibile la spec resta registrata
/// INATTIVA e ritenta lazy (mai shadow ramfs).
pub fn handle_mount(
    mounts: &mut Vec<mount::FsMount>,
    payload: &str,
    fgen: &mut u64,
    next_id: &mut u64,
) -> Result<u64, u64> {
    let mut parts = payload.split('\0');
    let source = parts.next().ok_or(ERR_INVALID)?;
    let target = parts.next().ok_or(ERR_INVALID)?;
    if source.is_empty() || target.is_empty() {
        return Err(ERR_INVALID);
    }
    if mount::apply_mount_spec(mounts, source, target, "", next_id) {
        // La tabella e' cambiata (spec nuova/sostituita): invalida le cache.
        *fgen = fgen.wrapping_add(1);
        Ok(0)
    } else {
        // apply fallisce a resolve sorgente (nome ignoto/driver morto) o a
        // target invalido: NotFound nel primo caso. Senza visibilita' interna,
        // NotFound e' il rifiuto piu' onesto (il client tipico ha sbagliato la
        // sorgente; il target malformato e' gia' filtrato sopra).
        Err(ERR_NOTFOUND)
    }
}

/// Smonta un target (Fase 16b). Rifiutato se ci sono fd aperti sotto il mount
/// (EBUSY); la radice ramfs non e' smontabile (non e' in tabella).
/// A rimozione riuscita bumpa `gen`. Gli fd tengono mount-id (Fase 49, F2):
/// orfani di un umount riuscito danno errore al prossimo uso invece di
/// aliasare il vicino shiftato.
pub fn handle_umount(
    mounts: &mut Vec<mount::FsMount>,
    ftable: &ftable::FileTable,
    target: &str,
    fgen: &mut u64,
) -> Result<u64, u64> {
    let norm = mount::normalize_target(target).ok_or(ERR_INVALID)?;
    let mid = mounts.iter().find(|m| m.target == norm).map(|m| m.id).ok_or(ERR_NOTFOUND)?;
    if ftable.has_mount_users(mid) {
        return Err(ERR_BUSY);
    }
    let idx = mount::by_id(mounts, mid).ok_or(ERR_NOTFOUND)?;
    mounts.remove(idx);
    *fgen = fgen.wrapping_add(1);
    Ok(0)
}

/// Sposta l'offset di un fd LOCALE (Fase 40, R_LSEEK): `off` con segno,
/// `whence` = SEEK_SET/CUR/END. Solo Local (Remote → ERR_INVALID: l'offset
/// vive in userfs, i driver non lo conoscono). Ritorna il nuovo offset.
/// Two-phase: valida tutto PRIMA di `set_offset` (a rifiuto l'offset resta
/// quello di prima, mai stato intermedio).
pub fn handle_lseek(
    fs: &ramfs::RamFs,
    ftable: &mut ftable::FileTable,
    mounts_fat: &mut Vec<mount::FsMount>,
    chan: u64,
    fd: u32,
    off: i64,
    whence: u64,
    fgen: &mut u64,
) -> Result<u64, u64> {
    // `get` ritorna Some solo per i Local (Remote e fd ignoti → Invalid:
    // niente EBADF nel nativo; l'offset vive in userfs, i driver non lo
    // conoscono).
    let (path, kind, cur, mnt) = ftable.get(chan, fd).ok_or(ERR_INVALID)?;
    let base: i64 = match whence {
        libr::SEEK_SET => 0,
        libr::SEEK_CUR => cur as i64,
        libr::SEEK_END => {
            let size = match kind {
                mount_legacy::FsKind::Ram => match fs.find(path).ok_or(ERR_NOTFOUND)? {
                    ramfs::FsNode::File { data, .. } => data.len() as i64,
                    // Dir: size 0 (lseek lecito, le read restano IsDir).
                    _ => 0,
                },
                mount_legacy::FsKind::Local => {
                    // Size fresca via stat (Fase 49, F4): niente cache.
                    let mid = mnt.ok_or(ERR)?;
                    let d = mount::by_id_mut(mounts_fat, mid).ok_or(ERR)?.local_dyn().ok_or(ERR)?;
                    let rel_owned: String = alloc::string::String::from(path);
                    match d.stat_dyn(&rel_owned).map_err(|_| ERR_NOTFOUND)? {
                        meta if meta.kind == 1 => 0,
                        meta => meta.size as i64,
                    }
                }
                mount_legacy::FsKind::Fat => {
                    let mid = mnt.ok_or(ERR)?;
                    if !mount::reactivate_mount_by_id(mounts_fat, mid, fgen) {
                        return Err(ERR);
                    }
                    let g = *fgen;
                    let fat = mount::by_id(mounts_fat, mid)
                        .and_then(|i| mounts_fat.get(i))
                        .ok_or(ERR)?
                        .fat()
                        .ok_or(ERR)?;
                    match ftable::fd_fat_info(ftable, fat, chan, fd, g).ok_or(ERR_NOTFOUND)? {
                        info if info.is_dir => 0,
                        info => info.size as i64,
                    }
                }
            };
            size
        }
        _ => return Err(ERR_INVALID),
    };
    let new = base.checked_add(off).ok_or(ERR_INVALID)?;
    if new < 0 {
        return Err(ERR_INVALID);
    }
    // Oltre EOF lecito (le read tornano 0, le write crescono): solo >= 0.
    ftable.set_offset(chan, fd, new as usize);
    Ok(new as u64)
}
