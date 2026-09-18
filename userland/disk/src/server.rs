use super::*;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[userdisk] starting, pid={}", libr::getpid());

    // 1. Rilevamento (solo HW, niente FS coinvolto).
    let mut infos = Vec::new();
    let atapi = detect::detect(&mut infos);
    let mut disks = Vec::new();
    for (i, info) in infos.iter().enumerate() {
        let letter = (b'a' + i as u8) as char;
        let model = core::str::from_utf8(&info.model[..info.model_len]).unwrap_or("?");
        println!(
            "[userdisk] sd{}: {} settori, {} {} ({})",
            letter,
            info.sectors,
            if info.lba48 { "LBA48" } else { "LBA28" },
            if info.cmd == 0x1F0 { "primary" } else { "secondary" },
            if info.drive == 0 { "master" } else { "slave" }
        );
        println!("[userdisk] sd{}: modello '{}'", letter, model);
        let serial = core::str::from_utf8(&info.serial[..info.serial_len]).unwrap_or("?");
        println!("[userdisk] sd{}: seriale '{}'", letter, serial);
        disks.push(block::AtaDisk::open(info.cmd, info.drive, info.lba48));
    }
    if atapi > 0 {
        println!("[userdisk] {} device ATAPI skippati (PACKET futuro)", atapi);
    }
    if disks.is_empty() {
        println!("[userdisk] nessun disco ATA: solo registrazione servizio");
    }

    // 2. Nodi: whole-disk + partizioni MBR primarie (graceful se assenti).
    // Handle = disco<<16|sub, allocato QUI (Fase 16c): la tabella `nodes' e'
    // la single source of truth nome→handle; userfs lo chiede con DISK_RESOLVE.
    let mut nodes: Vec<nodes::Node> = Vec::new();
    let mut disk_sectors: Vec<u64> = Vec::new();
    let mut parts: Vec<Vec<nodes::PartLoc>> = Vec::new();
    for (i, disk) in disks.iter().enumerate() {
        let letter = (b'a' + i as u8) as char;
        disk_sectors.push(infos[i].sectors);
        // Identità del whole-disk dallo stesso settore 0 (Fase 16d).
        let (wd_uuid, wd_label) = nodes::sniff_identity(disk, 0);
        nodes.push(nodes::Node {
            name: alloc::format!("sd{}", letter),
            handle: (i as u32) << 16,
            vol_uuid: wd_uuid,
            vol_label: wd_label,
        });
        let mut disk_parts: Vec<nodes::PartLoc> = Vec::new();
        let mut sec0 = [0u8; 512];
        if disk.read_sector(0, &mut sec0) {
            let mut parsed = Vec::new();
            part::parse_mbr(&sec0, &mut parsed);
            for (p, part) in parsed.iter().enumerate() {
                println!(
                    "[userdisk] sd{}{}: tipo {:#04x}, start {}, settori {}",
                    letter,
                    p + 1,
                    part.ptype,
                    part.start,
                    part.sectors
                );
                // Identità della partizione dal suo boot sector (Fase 16d:
                // un settore in più per partizione, solo a boot).
                let (pu, pl) = nodes::sniff_identity(disk, part.start as u64);
                nodes.push(nodes::Node {
                    name: alloc::format!("sd{}{}", letter, p + 1),
                    handle: ((i as u32) << 16) | (p as u32 + 1),
                    vol_uuid: pu,
                    vol_label: pl,
                });
                disk_parts.push(nodes::PartLoc { start: part.start, sectors: part.sectors });
            }
        }
        parts.push(disk_parts);
    }

    // Prefix da registrare presso userfs (Fase 16d): il nodo + gli alias
    // stabili che ha (`/dev/disk/by-uuid/<HEX8>`, `/dev/disk/by-label/<NOME>`).
    // La FsReg li consuma in ordine; userfs li tratta come prefix qualunque
    // (open esatto + listing sintetizzato dalla Mount table, B4).
    let mut reg_prefixes: Vec<String> = Vec::new();
    for n in nodes.iter() {
        reg_prefixes.push(alloc::format!("/dev/{}", n.name));
        if let Some(u) = n.vol_uuid {
            reg_prefixes.push(alloc::format!("/dev/disk/by-uuid/{:08X}", u));
        }
        if let Some(l) = &n.vol_label {
            reg_prefixes.push(alloc::format!("/dev/disk/by-label/{}", l));
        }
        // Riga identità per-nodo (Fase 16d): umana + asserzione host-side
        // del reorder (test-uuid-reorder.py cerca `uuid=<U2>` sulla lettera).
        let mut idline = alloc::format!("[userdisk] {}: handle={:#x}", n.name, n.handle);
        if let Some(u) = n.vol_uuid {
            idline.push_str(&alloc::format!(" uuid={:08X}", u));
        }
        if let Some(l) = &n.vol_label {
            idline.push_str(&alloc::format!(" label='{}'", l));
        }
        println!("{}", idline);
    }

    // 3. Ring FS + DISK dedicati (allocazione raw, MAI via libr::fs_init che e'
    // sincrono): FS per BUF_REG/REGISTER async, DISK per il data-plane con
    // userfs. Retry throttled: senza, niente registrazione ne' data-plane.
    // Reset head=tail: le pagine devono partire allineate.
    let (fs_req_phys, fs_resp_phys) = loop {
        if let Some(pair) = libr::ring_alloc_raw() {
            break pair;
        }
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
    };
    let (disk_req_phys, disk_resp_phys) = loop {
        if let Some(pair) = libr::ring_alloc_raw() {
            break pair;
        }
        for _ in 0..1_000_000 {
            core::hint::spin_loop();
        }
    };
    if libr::map_physical(fs_req_phys, FS_REQ_VA, 1).is_err()
        || libr::map_physical(fs_resp_phys, FS_RESP_VA, 1).is_err()
        || libr::map_physical(disk_req_phys, DISK_REQ_VA, 1).is_err()
        || libr::map_physical(disk_resp_phys, DISK_RESP_VA, 1).is_err()
    {
        println!("[userdisk] map ring fallita, exit");
        libr::exit(1);
    }
    rings::fs_rings_reset();
    unsafe {
        core::ptr::write_volatile((DISK_REQ_VA + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((DISK_REQ_VA + RING_TAIL as u64) as *mut u32, 0);
        core::ptr::write_volatile((DISK_RESP_VA + RING_HEAD as u64) as *mut u32, 0);
        core::ptr::write_volatile((DISK_RESP_VA + RING_TAIL as u64) as *mut u32, 0);
    }

    // 4. Servizio Disk per nome (ADR-0008): userfs lo risolve per il
    // data-plane, init per la supervisione, il kernel non instrada IRQ.
    if libr::service_register(libr::Service::Disk).is_ok() {
        println!("[userdisk] registered as service Disk");
    }

    // 5. READY al parent SUBITO (come console): userdisk parte PRIMA di userfs
    // (16.3) e l'ACK non puo' aspettare il mount (deadlock: il mount aspetta
    // Fs che parte dopo). Fire-and-forget in `libr` (A3), retry bounded, mai hang.
    libr::signal_ready(1);

    // 6. Registrazione FS via SM async (mai sync: vedi doc in testa). DISK e
    // DEV funzionano anche a registrazione incompleta: userfs monta appena
    // HELLO risponde, senza aspettare i prefix.
    let mut fsreg = fs_reg::FsReg::new(fs_req_phys, fs_resp_phys);

    // fd DEV_* (raw sequenziale) → (handle nodo, posizione in byte).
    let mut fds: BTreeMap<u32, (u32, u64)> = BTreeMap::new();
    let mut next_fd: u32 = 1;

    loop {
        // Invio nella stessa chiamata (lezione tty): prima di dormire in recv
        // bisogna aver notificato, altrimenti nessuno ci sveglia.
        fsreg.step(&reg_prefixes);
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Reply async FS (BUF_REG/REGISTER): consuma per primo, prima di ogni
        // dispatch (req_id > 0 solo per le risposte, mai per le richieste).
        if fsreg.collect_if_mine(msg.req_id, &reg_prefixes) {
            continue;
        }

        // userfs morto e rinato: reset SM (re-handshake + re-register). I ring
        // DISK persistono (pagine proprie): userfs rifa' HELLO da solo. Niente
        // send sincrone qui: solo reset di stato. Mai reply (peer morto).
        if msg.tag == libr::EXIT_NOTIFY {
            fsreg.reset();
            continue;
        }

        // ── Data-plane DISK_* (canale diretto userfs) ──
        if msg.tag == DISK_HELLO {
            // Fisici nei registri di reply (tag 0, mai !0 = ERR): niente frame.
            let _ = libr::reply(0, disk_req_phys, disk_resp_phys);
            continue;
        }
        if msg.tag == DISK_OPEN {
            if nodes::locate(msg.w0 as u32, &disk_sectors, &parts).is_some() {
                let _ = libr::reply(0, 0, 0);
            } else {
                let _ = libr::reply(0, ERR, 0);
            }
            continue;
        }
        if msg.tag == DISK_READ {
            // P1.2 — richiesta multi: frame `[count:8]`, risposta con
            // count*512 byte in UN frame (1 IPC invece di count).
            let handle = msg.w0 as u32;
            let lba = msg.w1;
            let mut buf = [0u8; nodes::DISK_MAX_SECTORS * 512];
            match rings::disk_req_read_count() {
                Some(n)
                    if nodes::node_read_multi(
                        &disks,
                        &disk_sectors,
                        &parts,
                        handle,
                        lba,
                        &mut buf[..n * 512],
                    ) =>
                {
                    unsafe { rings::disk_resp_write((n * 512) as u64, 0, &buf[..n * 512]) };
                    let _ = libr::reply(0, 0, 0);
                }
                _ => {
                    let _ = libr::reply(0, ERR, 0);
                }
            }
            continue;
        }
        if msg.tag == DISK_CLOSE {
            let _ = libr::reply(0, 0, 0);
            continue;
        }
        if msg.tag == DISK_WRITE {
            // P1.2 — frame `[count:8][count*512 byte]`, 1 comando PIO + 1
            // flush per l'intero run (prima: comando+flush a settore).
            // Handle in w0, lba in w1. Frame consumato sempre, anche a
            // handle/lba invalidi (stesso contratto dei ring FS).
            let handle = msg.w0 as u32;
            let lba = msg.w1;
            let mut buf = [0u8; nodes::DISK_MAX_SECTORS * 512];
            let ok = match rings::disk_req_read_multi(&mut buf) {
                Some(n) => nodes::node_write_multi(
                    &disks,
                    &disk_sectors,
                    &parts,
                    handle,
                    lba,
                    &buf[..n * 512],
                ),
                None => false,
            };
            if ok {
                let _ = libr::reply(0, 0, 0);
            } else {
                let _ = libr::reply(0, ERR, 0);
            }
            continue;
        }
        if msg.tag == DISK_RESOLVE {
            // Single source of truth nome→handle (Fase 16c, identità 16d):
            // la chiave ("sda", UUID hex, label) arriva nel frame DISK_REQ,
            // l'handle torna in w0. Sconosciuto/malformato → ERR, mai frame.
            let result = match rings::disk_req_read_name() {
                Some(key) => nodes::resolve_node(&nodes, &key).map(|n| n.handle as u64),
                None => None,
            };
            let _ = libr::reply(0, result.unwrap_or(ERR), 0);
            continue;
        }

        // ── Relay DEV_* (open raw /dev/sdX dai client via userfs) ──
        // w0 di DEV_OPEN = handle codificato (disco<<16|sub, 0 = whole):
        // userfs-16.2 lo ricava parsando il nome Linux ("sda"→0, "sda1"→1),
        // senza bisogno della lista nodi. La posizione avanza a ogni READ.
        let result: Option<u64> = match msg.tag {
            DEV_OPEN => {
                let handle = msg.w0 as u32;
                if nodes::locate(handle, &disk_sectors, &parts).is_some() {
                    let fd = next_fd;
                    next_fd += 1;
                    fds.insert(fd, (handle, 0));
                    Some(fd as u64)
                } else {
                    None
                }
            }
            DEV_READ => {
                let (handle, pos) = match fds.get(&(msg.w0 as u32)) {
                    Some(&p) => p,
                    None => {
                        let _ = libr::reply(0, ERR, 0);
                        continue;
                    }
                };
                let node_sectors = match nodes::locate(handle, &disk_sectors, &parts) {
                    Some((_, _, s)) => s,
                    None => {
                        let _ = libr::reply(0, ERR, 0);
                        continue;
                    }
                };
                let avail = node_sectors * 512 - pos.min(node_sectors * 512);
                let want = (msg.w1 as u64).min(avail).min(4096);
                let nsec = (want / 512) as usize;
                if nsec == 0 {
                    // EOF o count < 512: frame vuoto + 0 (come /dev/null), mai
                    // wedge il client (lezione fix kbd/tty).
                    unsafe { resp_frame_write(CLI_RESP, &[]) };
                    Some(0)
                } else {
                    let mut buf = [0u8; 4096];
                    let mut ok = 0usize;
                    for s in 0..nsec {
                        let lba = pos / 512 + s as u64;
                        let mut sec = [0u8; 512];
                        if !nodes::node_read(&disks, &disk_sectors, &parts, handle, lba, &mut sec) {
                            break;
                        }
                        buf[s * 512..(s + 1) * 512].copy_from_slice(&sec);
                        ok += 1;
                    }
                    if ok == 0 {
                        None
                    } else {
                        let n = ok * 512;
                        unsafe { resp_frame_write(CLI_RESP, &buf[..n]) };
                        fds.insert(msg.w0 as u32, (handle, pos + n as u64));
                        Some(n as u64)
                    }
                }
            }
            DEV_WRITE => {
                // Read-only: consuma comunque il payload (tail!) e rifiuta.
                let count = msg.w1 as usize;
                unsafe { req_frame_consume(CLI_REQ, count) };
                None
            }
            DEV_CLOSE => {
                if fds.remove(&(msg.w0 as u32)).is_some() {
                    Some(0)
                } else {
                    None
                }
            }
            DEV_READDIR => {
                // Nomi delle partizioni figlie del nodo (o vuoto). Il chiamante
                // e' userfs su relay del prefix stesso: rel non disponibile qui,
                // quindi si elencano i figli di TUTTI i dischi? No: senza rel,
                // risposta vuota conservativa (t32 usa open/read diretti).
                unsafe { resp_frame_write(CLI_RESP, &[]) };
                Some(0)
            }
            _ => None,
        };
        // Idempotente: reply ERR senza frame (convenzione driver), come
        // devfs/kbd — il client vede -1, mai wedge.
        let _ = libr::reply(0, result.unwrap_or(ERR), 0);
    }
}
