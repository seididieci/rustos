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

    // Filesystem locali: prima i mount FAT (con attivazione lazy), poi ramfs.
    if let Some((mi, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        if creat {
            if let Some(fat) = mounts_fat.get(mi).and_then(|m| m.fat()) {
                if fat.find(rel).is_none() {
                    let _ = fat.create_file(rel);
                    *fgen = fgen.wrapping_add(1);
                }
            }
        }
        let fat = mounts_fat[mi].fat().ok_or(ERR)?;
        let info = fat.find(rel).ok_or(ERR_NOTFOUND)?;
        if info.is_dir {
            return Err(ERR_ISDIR);
        }
        // O_TRUNC su FAT: libera la catena e azzera la entry (write_grow non
        // tronca). A fallimento: rifiuto, mai fd su file non troncato.
        if trunc {
            if !fat.truncate(&info) {
                return Err(ERR);
            }
            *fgen = fgen.wrapping_add(1);
            let info = fat.find(rel).ok_or(ERR_NOTFOUND)?;
            return Ok(ftable.open_fat(chan, rel, mi, info, *fgen, append));
        }
        return Ok(ftable.open_fat(chan, rel, mi, info, *fgen, append));
    }
    match mount_legacy::resolve_local(mounts_fat, path).ok_or(ERR_NOTFOUND)? {
        mount_legacy::FsKind::Fat => Err(ERR), // mount inattivo: errore, mai shadow ramfs
        mount_legacy::FsKind::Ram => {
            if creat {
                let _ = fs.create_file(path);
            }
            match fs.find(path).ok_or(ERR_NOTFOUND)? {
                ramfs::FsNode::File { .. } => {}
                ramfs::FsNode::Dir { .. } => return Err(ERR_ISDIR),
            }
            // O_TRUNC su ramfs: svuota il vettore (in-memoria, infallibile a
            // path esistente — appena verificato sopra).
            if trunc {
                if let Some(ramfs::FsNode::File { data, .. }) = fs.find_or_create(path) {
                    data.clear();
                }
            }
            Ok(ftable.open(chan, path, mount_legacy::FsKind::Ram, None, append))
        }
    }
}

pub fn handle_read(
    fs: &ramfs::RamFs,
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
            let d = match fs.find(path).ok_or(ERR_NOTFOUND)? {
                ramfs::FsNode::File { data: d, .. } => d,
                // Dir aperta (solo via fd pre-40): leggere una dir e' IsDir.
                _ => return Err(ERR_ISDIR),
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
            let mi = mnt.ok_or(ERR)?;
            // Il mount puo' essere caduto inattivo alla morte di userdisk
            // (drop d'epoca in `note_peer_death`): riattiva per nome qui, come
            // `resolve_fsmount` fa per open/readdir (fail-loud, mai shadow).
            if !mount::reactivate_mount(mounts_fat, mi, fgen) {
                return Err(ERR);
            }
            let g = *fgen;
            let fat = mounts_fat.get(mi).ok_or(ERR)?.fat().ok_or(ERR)?;
            let info = ftable::fd_fat_info(ftable, fat, chan, fd, g).ok_or(ERR_NOTFOUND)?;
            if info.is_dir {
                return Err(ERR_ISDIR);
            }
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
    if kind == mount_legacy::FsKind::Fat {
        // Scrittura FAT (Fase 20): overwrite + crescita con allocazione
        // (write-through, niente cache FileInfo: la scrittura puo' cambiare
        // size/first_cluster, quindi dopo si bumpa la generazione e si
        // riaggiorna la cache con un find fresco — un find per write, rumore
        // contro le centinaia di round-trip DISK della scrittura stessa).
        let mi = mnt.ok_or(ERR)?;
        if !mount::reactivate_mount(mounts_fat, mi, fgen) {
            return Err(ERR);
        }
        let g = *fgen;
        let fat = mounts_fat.get(mi).ok_or(ERR)?.fat().ok_or(ERR)?;
        let info = ftable::fd_fat_info(ftable, fat, chan, fd, g).ok_or(ERR_NOTFOUND)?;
        if info.is_dir {
            return Err(ERR_ISDIR);
        }
        // O_APPEND: l'offset del fd e' ignorato, si accoda a size corrente.
        let offset = if append { info.size as usize } else { offset };
        let n = fat.write_grow(&info, offset, &payload[..count.min(payload.len())]);
        *fgen = fgen.wrapping_add(1);
        let g2 = *fgen;
        // Rileggi l'entry dopo la mutazione (size/first_cluster possono aver
        // cambiato valore): la cache resta valida alla nuova generazione.
        let fat = mounts_fat.get(mi).ok_or(ERR)?.fat().ok_or(ERR)?;
        let rel: String = match ftable.files.get(&(chan, fd)) {
            Some(ftable::FileEntry::Local { path, kind: mount_legacy::FsKind::Fat, .. }) => path.clone(),
            _ => return Ok(n as u64),
        };
        let fresh = fat.find(&rel);
        ftable.refresh_fat_info(chan, fd, fresh, g2);
        ftable.set_offset(chan, fd, offset + n);
        return Ok(n as u64);
    }

    match fs.find_or_create(path).ok_or(ERR_NOTFOUND)? {
        ramfs::FsNode::File { data: file_data, .. } => {
            // O_APPEND: accoda a len corrente invece dell'offset del fd.
            let offset = if append { file_data.len() } else { offset };
            if offset + count > file_data.len() {
                file_data.resize(offset + count, 0);
            }
            file_data[offset..offset + count].copy_from_slice(&payload[..count]);
            ftable.set_offset(chan, fd, offset + count);
            Ok(count as u64)
        }
        // Dir aperta (solo via fd pre-40): scrivere una dir e' IsDir.
        _ => Err(ERR_ISDIR),
    }
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
    fs: &ramfs::RamFs,
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

    if let Some((mi, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        let fat = mounts_fat[mi].fat().ok_or(ERR)?;
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
        Some(mount_legacy::FsKind::Ram) => match fs.readdir(path) {
            Some(e) => (true, e),
            None => (false, Vec::new()),
        },
        // Mount noto ma inattivo: NotFound, mai shadow ramfs.
        Some(mount_legacy::FsKind::Fat) => return Err(ERR_NOTFOUND),
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
    if let Some((mi, rel)) = mount::resolve_fsmount(mounts_fat, path, fgen) {
        let fat = mounts_fat[mi].fat().ok_or(ERR)?;
        if rel.is_empty() {
            return Ok(stat_reply(rings, chan, 0, libr::STAT_DIR));
        }
        let info = fat.find(rel).ok_or(ERR_NOTFOUND)?;
        let kind = if info.is_dir { libr::STAT_DIR } else { libr::STAT_FILE };
        return Ok(stat_reply(rings, chan, info.size as u64, kind));
    }
    match mount_legacy::resolve_local(mounts_fat, path) {
        // Mount noto ma inattivo: errore, mai shadow ramfs.
        Some(mount_legacy::FsKind::Fat) => Err(ERR),
        Some(mount_legacy::FsKind::Ram) => match fs.find(path) {
            Some(ramfs::FsNode::File { data, .. }) => {
                Ok(stat_reply(rings, chan, data.len() as u64, libr::STAT_FILE))
            }
            Some(ramfs::FsNode::Dir { .. }) => {
                Ok(stat_reply(rings, chan, 0, libr::STAT_DIR))
            }
            // Non in ramfs: puo' essere un padre sintetizzato (sotto).
            None => mount_legacy::synth_children(mounts, path)
                .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR))
                .ok_or(ERR_NOTFOUND),
        },
        // /dev/* senza prefix noto: solo sintesi (sotto).
        None => mount_legacy::synth_children(mounts, path)
            .map(|_| stat_reply(rings, chan, 0, libr::STAT_DIR))
            .ok_or(ERR_NOTFOUND),
    }
}

pub fn handle_mkdir(fs: &mut ramfs::RamFs, mounts: &[mount::FsMount], path: &str) -> Result<u64, u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(ERR_INVALID);
    }
    // mkdir solo su ramfs (i mount FAT/remoti non hanno mkdir).
    match mount_legacy::resolve_local(mounts, path).ok_or(ERR_NOTFOUND)? {
        mount_legacy::FsKind::Ram => {
            // Distingue "esiste gia'" (Exists) da "padre mancante" (NotFound):
            // prima sonda, poi crea (single-thread: nessuna race tra i due).
            if fs.find(path).is_some() {
                return Err(ERR_EXISTS);
            }
            fs.mkdir(path).ok_or(ERR_NOTFOUND)?;
            Ok(0)
        }
        _ => Err(ERR),
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
) -> Result<u64, u64> {
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(ERR_INVALID);
    }
    // Mai dentro driver remoti…
    if mount_legacy::resolve_mount(path, mounts).is_some() {
        return Err(ERR_INVALID);
    }
    // …e mai su mount FAT (unlink non implementato: per questa op il volume
    // e' readonly): solo ramfs.
    match mount_legacy::resolve_local(mounts_fat, path).ok_or(ERR_NOTFOUND)? {
        mount_legacy::FsKind::Ram => {
            fs.remove(path).ok_or(ERR_NOTFOUND)?;
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
pub fn handle_mount(mounts: &mut Vec<mount::FsMount>, payload: &str, fgen: &mut u64) -> Result<u64, u64> {
    let mut parts = payload.split('\0');
    let source = parts.next().ok_or(ERR_INVALID)?;
    let target = parts.next().ok_or(ERR_INVALID)?;
    if source.is_empty() || target.is_empty() {
        return Err(ERR_INVALID);
    }
    if mount::apply_mount_spec(mounts, source, target, "") {
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
/// A rimozione riuscita bumpa `gen` (gli indici mount degli fd restanti non
/// cambiano per EBUSY, ma le istanze vanno comunque ricontrollate).
pub fn handle_umount(
    mounts: &mut Vec<mount::FsMount>,
    ftable: &ftable::FileTable,
    target: &str,
    fgen: &mut u64,
) -> Result<u64, u64> {
    let norm = mount::normalize_target(target).ok_or(ERR_INVALID)?;
    let idx = mounts.iter().position(|m| m.target == norm).ok_or(ERR_NOTFOUND)?;
    if ftable.has_mount_users(idx) {
        return Err(ERR_BUSY);
    }
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
                mount_legacy::FsKind::Fat => {
                    let mi = mnt.ok_or(ERR)?;
                    if !mount::reactivate_mount(mounts_fat, mi, fgen) {
                        return Err(ERR);
                    }
                    let g = *fgen;
                    let fat = mounts_fat.get(mi).ok_or(ERR)?.fat().ok_or(ERR)?;
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
