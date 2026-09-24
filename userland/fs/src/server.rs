use super::*;

// ── Main ───────────────────────────────────────────────────────────

/// Nome del driver dietro il canale `chan` dal suo `image_hash` (Fase 36,
/// identita' misurata): confronto col manifest generato a build-time; `"?"`
/// se il canale e' morto o l'hash e' ignoto (test/helper, userfs stesso:
/// `HASH_USERFS` non esiste — userfs incorpora il manifest e il suo hash
/// sarebbe un ciclo). Solo diagnostica nei log, mai decisioni (la policy
/// confronta gli hash, non i nomi).
fn driver_name_of(chan: u64) -> &'static str {
    match libr::peer_info(chan) {
        Ok(h) if h == HASH_USERCONSOLE => "userconsole",
        Ok(h) if h == HASH_USERDEVFS => "userdevfs",
        Ok(h) if h == HASH_USERDISK => "userdisk",
        Ok(h) if h == HASH_USERKBD => "userkbd",
        Ok(h) if h == HASH_USERSHELL => "usershell",
        Ok(h) if h == HASH_USERTTY => "usertty",
        Ok(h) if h == HASH_USERUPTIME => "useruptime",
        _ => "?",
    }
}

libr::entry!(real_main);
fn real_main(_sp: u64) -> ! {
    println!("[userfs] starting");

    // Registra il servizio Fs SUBITO (ADR-0008): il mount FAT32 e' lento, e i
    // client (devfs, testfs) risolvono Fs per nome appena partono. Registrarsi
    // prima del mount evita che chi spawa dopo aspetti inutilmente.
    // (L'ACK READY a init parte invece DOPO il populate, prima del loop:
    // READY significa "davvero pronto".)
    let reg_ok = libr::service_register(libr::Service::Fs).is_ok();
    if reg_ok {
        println!("[userfs] registered as service Fs");
    } else {
        println!("[userfs] FAILED to register service Fs");
    }

    // Mount FAT32 dalle spec statiche (Fase 16b: stesso codice dei mount
    // dinamici; IpcDisk riconnette da solo a ogni restart di userdisk, quindi
    // i mount sopravvivono alla morte del driver — t32). Spec inattive
    // (disco assente) restano in tabella e ritentano lazy al primo accesso.
    let mut fat_mounts: Vec<mount::FsMount> = Vec::new();
    for (src, tgt) in mount::STATIC_MOUNTS {
        if mount::apply_mount_spec(&mut fat_mounts, src, tgt, "") {
            println!("[userfs] FAT32 montato a /{} (via userdisk)", tgt);
        } else {
            println!("[userfs] mount {} -> {} inattivo (disco assente?)", src, tgt);
        }
    }
    if fat_mounts.iter().all(|m| !m.is_active()) {
        println!("[userfs] nessun FAT attivo: ramfs only");
    }

    let mut fs = ramfs::RamFs::new();
    let mut ftable = ftable::FileTable::new();
    let mut mounts: Vec<mount_legacy::Mount> = Vec::new();
    // Generazione delle cache FileInfo per-fd (vedi `FileEntry`): bumpata a
    // ogni mutazione FAT (write/create/mount/umount/remount/drop d'epoca).
    // Parte da 1 (0 = mai usato, come le entry appena create per ramfs).
    let mut fat_gen: u64 = 1;

    // Client registrati: pid → (req_ring_phys, resp_ring_phys).
    let mut rings: BTreeMap<u64, (u64, u64)> = BTreeMap::new();

    // Diritti per-canale (Fase 17): entry assente = {ALL, root}.
    let mut rights: BTreeMap<u64, rights::ChanRights> = BTreeMap::new();
    // Tetto policy su identita' (Fase 45): chan → mask massima, classificato
    // UNA volta all'handshake (peer_info), purgato all'EXIT_NOTIFY come
    // rings/rights. Effettivo = drop_mask & ceiling (mai widen).
    let mut policy: BTreeMap<u64, u32> = BTreeMap::new();

    // Grant single-use per handoff fd (Fase 40, modello B): nonce → snapshot.
    let mut grants = dup::GrantTable::new();

    // Pipe buffer server-side (Fase 42): id → buffer condiviso tra gli fd
    // delle due estremita' (conteggio estremita', libera all'ultima close).
    let mut pipes = pipes::PipeTable::new();

    // Pre-populate: file di esempio
    if let Some(data) = fs.create_file("hello.txt") {
        data.extend_from_slice(b"Hello from Velordor ramfs!\n");
    }
    if let Some(data) = fs.create_file("test.txt") {
        data.extend_from_slice(b"Line 1\nLine 2\nLine 3\n");
    }
    println!("[userfs] ramfs popolata, entro in loop");

    // Notifica a init (canale di nascita) che il servizio Fs e' pronto: init
    // aspetta questo ACK prima di spawnare chi usa il filesystem (boot
    // deterministico, ADR-0008). DOPO il populate: READY significa "davvero
    // pronto" (vale anche per i restart: init procede a filesystem completo).
    // READY fire-and-forget (send_async, init-restart): init consuma senza
    // reply → una send sync resterebbe bloccata. Retry bounded, mai hang.
    libr::signal_ready(reg_ok as u64);

    // 24.2-diagnosi: contatore rimosso (era temporaneo); heap_stats resta in
    // libr per future diagnosi.
    loop {
        let msg = match libr::recv() {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Scratch arena per-op (libr): TUTTI i borrow sotto muoiono entro
        // questa iterazione (handler sincroni, reply prima del prossimo
        // recv). Mai tenere `&` scratch oltre il fondo del loop.
        libr::scratch::reset();

        // Il client e' identificato dal canale da cui arriva la richiesta
        // (ADR-0008): ogni client ha il proprio canale verso Fs. La reply e'
        // implicita al messaggio corrente: se il client era bloccato in `send`
        // (sync) il kernel la consegna nel reply_slot; se era async (Fase 13)
        // il kernel accoda la risposta con req_id negativo. userfs non cambia.
        let chan = msg.channel;
        let tag = msg.tag;

        // Handshake ring buffer: il client registra i propri indirizzi fisici.
        if tag == FS_BUF_REG {
            rings.insert(chan, (msg.w0, msg.w1));
            // Fase 45: classifica il peer ORA (tetto in cache, niente
            // syscall per-op). L'handshake resta aperto a tutti: la policy
            // nega op, mai la registrazione (libr ritenta comunque).
            let ceil = policy::ceiling_for(chan);
            policy.insert(chan, ceil);
            println!("[userfs] client chan {} registered rings req={:#x} resp={:#x} ceiling={:#x}", chan, msg.w0, msg.w1, ceil);
            let _ = libr::reply(0, 0, 0);
            continue;
        }

        // Registrazione driver: un processo notifica il proprio prefix di mount.
        // Il prefix viaggia nel request ring del chiamante.
        if tag == FS_REGISTER {
            let (req_phys, _resp_phys) = match rings.get(&chan) {
                Some(&r) => r,
                // Senza handshake (server riavviato): il driver rifa FS_BUF_REG
                // e ripete (libr, come sopra).
                None => { let _ = libr::reply(0, ERR_NOHANDSHAKE, 0); continue; }
            };
            // Mappa il request ring del driver per leggere il prefix
            if libr::map_physical(req_phys, rings::REQ_RING_VA, 1).is_err() {
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
            let (op_tag, _w0, _w1, payload_len) = match rings::req_ring_read() {
                Some(f) => f,
                None => { let _ = libr::reply(0, ERR, 0); continue; }
            };
            if op_tag == R_REGISTER && payload_len > 0 && payload_len <= 514 {
                let mut prefix_buf = [0u8; 514];
                rings::req_ring_read_payload(&mut prefix_buf, payload_len);
                // Fase 35 (hardening) + Fase 36 (identita' misurata, Strato 2):
                // policy sui mount registrati dinamicamente.
                // (a) solo sotto `/dev/` (niente hijack di `/` o voci rogue a
                // root); (b) il replace di un prefix esistente solo dallo
                // STESSO binario (stesso `image_hash` del driver vivo: il
                // restart da disco rilegge gli stessi byte, quindi riesce
                // senza init) o da un figlio di init (bootstrap): impedisce a
                // un processo qualsiasi di squattare `/dev/null` dopo un kill.
                let caller = libr::peer_pid(chan).unwrap_or(-1);
                let init_child = caller >= 0
                    && matches!(libr::ps_info(caller as u32), Some(e) if e.parent == Some(1));
                // Payload = uno o piu' prefix NUL-separati (Fase 16d): devfs
                // registra "/dev/null\0/dev/zero" con UNA sola IPC, cosi' non
                // esiste una finestra in cui un mount e' forwardable mentre il
                // driver e' ancora bloccato in un secondo register sincrono.
                for raw in prefix_buf[..payload_len].split(|&b| b == 0) {
                    if raw.is_empty() {
                        continue;
                    }
                    let prefix = match core::str::from_utf8(raw) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    if !prefix.starts_with("/dev/") {
                        println!("[userfs] FS_REGISTER rifiutato: '{}' fuori /dev/", prefix);
                        continue;
                    }
                    // Idempotente sul prefix (init-restart): se il prefix era
                    // gia' registrato (driver morto non ancora purgato o double
                    // register), sostituisci invece di duplicare — lo stale
                    // avvelenerebbe resolve_mount (first-match). Fase 35: il
                    // replace di un prefix di un driver VIVO richiede un figlio
                    // di init; un driver morto (canale invalidato) puo' sempre
                    // essere rimpiazzato (riconnessione legittima). Fase 36: il
                    // replace riesce anche dallo STESSO binario (hash uguale a
                    // quello del driver vivo — restart da disco senza init).
                    let existing = mounts.iter().find(|m| m.prefix.as_str() == prefix).map(|m| m.driver_chan);
                    let stale = match existing {
                        Some(dc) => libr::peer_pid(dc).is_err(), // driver morto
                        None => true,
                    };
                    // Stesso binario? Solo a driver vivo e non-init-child (nei
                    // casi facili la risposta e' gia' nota: niente syscall).
                    let same_image = match existing {
                        Some(dc) if !stale && !init_child => {
                            match (libr::peer_info(chan), libr::peer_info(dc)) {
                                (Ok(a), Ok(b)) => a == b,
                                _ => false,
                            }
                        }
                        _ => false,
                    };
                    if existing.is_some() && !init_child && !stale && !same_image {
                        println!(
                            "[userfs] FS_REGISTER replace '{}' rifiutato (pid {} {}, driver vivo {})",
                            prefix, caller, driver_name_of(chan), driver_name_of(existing.unwrap()),
                        );
                        continue;
                    }
                    mounts.retain(|m| m.prefix.as_str() != prefix);
                    mounts.push(mount_legacy::Mount {
                        prefix: String::from(prefix),
                        driver_chan: chan,
                    });
                    println!("[userfs] registered mount '{}' → driver_chan={} ({})", prefix, chan, driver_name_of(chan));
                }
            } else {
                rings::req_ring_consume(20 + payload_len);
            }
            // Mappa il response ring del client per scrivere la risposta
            rings::map_client_resp_ring(&rings, chan);
            rings::resp_ring_write(0, 0, &[]);
            let _ = libr::reply(0, 0, 0);
            continue;
        }

        // Morte di un peer (client o driver): purga tutto lo stato per-canale
        // (notifica unificata, Fase 14). Senza reply: il peer e' morto.
        if tag == libr::EXIT_NOTIFY {
            let mut remotes = Vec::new();
            let mut pipe_ends = Vec::new();
            ftable.purge(chan, &mut remotes, &mut pipe_ends);
            for (srv, rfd) in remotes {
                // Best-effort: il driver potrebbe essere morto a sua volta.
                let _ = libr::send(srv, DEV_CLOSE, rfd as u64, 0);
            }
            // Estremita' pipe del morto: decrementa (l'ultima libera il
            // buffer; i peer vivi vedono EOF/Closed invece di un hang).
            for (pipe_id, write) in pipe_ends {
                pipes.end_closed(pipe_id, write);
            }
            rings.remove(&chan);
            // Diritti effimeri (Fase 17): col peer muore anche la sua riga —
            // al re-handshake riparte da default {ALL, root} (limite dichiarato).
            rights.remove(&chan);
            // Tetto policy (Fase 45): stessa vita — al re-handshake il peer
            // viene riclassificato (hash rimisurato allo spawn, mai stale).
            policy.remove(&chan);
            // Grant orfani del morto (Fase 40): un pid riusato non deve poter
            // riscuotere grant altrui (la doppia attestazione al claim chiude
            // comunque la race, ma senza residui non c'e' race).
            grants.purge_registrant(chan, &mut pipes);
            // Se il morto era un driver, i suoi mount tornano registrabili:
            // lo stale, primo in lista, avvelenerebbe resolve_mount anche
            // dopo una re-registrazione dello stesso prefix.
            mounts.retain(|m| m.driver_chan != chan);
            // Se il morto era userdisk, invalida i client disco di tutti i
            // mount (Fase 16c: drop d'epoca — il prossimo accesso re-risolve
            // per nome e rimonta, t32). Veloce: solo compare dentro IpcDisk.
            // Le istanze cambiano: bumpa la generazione delle cache FileInfo.
            let mut epoch_dropped = false;
            for m in fat_mounts.iter_mut() {
                if m.note_peer_death(chan) {
                    epoch_dropped = true;
                }
            }
            if epoch_dropped {
                fat_gen = fat_gen.wrapping_add(1);
            }
            continue;
        }

        // Ogni altra operazione deve essere un FS_NOTIFY.
        if tag != FS_NOTIFY {
            // Tag ignoto: ERR come prima (nessun cambio semantico).
            let _ = libr::reply(0, ERR, 0);
            continue;
        }
        // Client senza handshake ring (es. server riavviato dopo la sua
        // registrazione, t28): segnale dedicato cosi' il client rifa
        // FS_BUF_REG e ripete l'op UNA volta (libr, Fase 14).
        if !rings.contains_key(&chan) {
            let _ = libr::reply(0, ERR_NOHANDSHAKE, 0);
            continue;
        }
        // Mappa i ring del client nello spazio di userfs.
        if !rings::map_client_req_ring(&rings, chan) || !rings::map_client_resp_ring(&rings, chan) {
            let _ = libr::reply(0, ERR, 0);
            continue;
        }

        // Leggi l'header del request frame (senza consumare: la lunghezza vera
        // e' dichiarata in w0/w1, vedi sotto).
        let (op_tag, w0, w1, avail) = match rings::req_ring_read() {
            Some((t, a, b, avail)) => (t, a, b, avail),
            None => {
                // Ring vuoto a notifica arrivata: spuria/stale, niente da
                // consumare e niente da riallineare (tail==head gia').
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        };

        // Lunghezza payload dichiarata dal frame. Il formato frame non ha
        // lunghezza esplicita: consumare "tutto il disponibile" inghiotte gli
        // eventuali frame successivi gia' presenti (coalescenza), disallineando
        // il ring per sempre (osservato: tag=0x0 con pay enorme, RINGFULL lato
        // client e stallo senza recovery). Si consuma ESATTAMENTE il dichiarato;
        // il resto resta per la propria notifica.
        let expect: usize = match op_tag {
            R_OPEN | R_MKDIR | R_READDIR | R_REGISTER | R_MOUNT | R_UMOUNT | R_DELETE | R_STAT => {
                w0 as usize
            }
            R_WRITE | R_RIGHTS_DROP => w1 as usize,
            // LSEEK: payload 1 byte = whence (fd in w0, offset in w1).
            R_LSEEK => 1,
            // CLAIM/CANCEL: payload `[nonce:8]`; GRANT solo registri (w0 = fd).
            R_DUP_CLAIM | R_DUP_CANCEL => 8,
            // PIPE_CREATE: nessun payload (w0 = hint capacita').
            R_PIPE_CREATE => 0,
            R_READ | R_CLOSE | R_RIGHTS_GET | R_DUP_GRANT => 0,
            _ => {
                // Tag impossibile: scarta tutto e riallinea (vedi req_resync).
                rings::req_resync();
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        };
        if expect > 4096 || avail < expect {
            // Frame impossibile o incompleto: riallinea e fallisci
            // visibilmente (mai wedge). Il payload perso appartiene a un'epoca
            // disallineata; il client ritenta (tty) o vede -1 (sync).
            rings::req_resync();
            let _ = libr::reply(0, ERR, 0);
            continue;
        }

        // Diritti per-canale, check ops (Fase 17 + tetto Fase 45): CENTRALE,
        // prima di qualunque contatto handler/driver. A diniego il frame va
        // comunque consumato (20 + expect esatti) o il prossimo request del
        // client legge spazzatura — vale anche per il WRITE remoto negato
        // (mai map_in/send al driver in quel caso). Fallback fail-closed a
        // canale senza handshake-policy (non dovrebbe accadere: FS_NOTIFY
        // richiede rings, che richiede handshake).
        // Tetto policy in cache (Fase 45): niente syscall qui dentro.
        let ceiling = policy.get(&chan).copied().unwrap_or(policy::DEFAULT_UNKNOWN_OPS);
        if let Some(bit) = rights::op_bit(op_tag) {
            if rights::rights_ops(&rights, chan) & ceiling & bit == 0 {
                rings::req_ring_consume(20 + expect);
                rings::resp_ring_write(ERR, 0, &[]);
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        }

        // R_WRITE verso un device remoto: NON consumare il request frame. Il
        // payload resta nel request ring del client e il driver (console/devfs),
        // che ha i ring del client iniettati via map_in, lo legge direttamente e
        // avanza la tail di (20 + w1) esatti. Qui scriviamo solo il result frame.
        if op_tag == R_WRITE && ftable.get_remote(chan, w0 as u32).is_some() {
            let result = handlers::handle_write_remote(&ftable, &rings, chan, w0 as u32, w1 as usize);
            rings::resp_ring_write(mount::to_reply_res(result), 0, &[]);
            let _ = libr::reply(0, mount::to_reply_res(result), 0);
            continue;
        }

        // Percorsi locali (o remote non-WRITE): consuma ESATTAMENTE header +
        // payload dichiarato. req_ring_read_payload avanza la tail di
        // (20 + expect); eventuali byte successivi (coalescenza) restano per
        // la loro notifica invece di essere inghiottiti.
        // Il payload vive in scratch (mai heap: e' il temp per-op piu' grosso,
        // fino a 4096 B per chunk di write; consumato entro l'iterazione).
        // `expect` ≤ 4096 per il bound sopra: sta nel backing iniziale.
        let payload: &mut [u8] = match libr::scratch::alloc_bytes(expect) {
            Some(p) => p,
            None => {
                // OOM vera sullo scratch: come frame impossibile (mai wedge).
                rings::req_resync();
                let _ = libr::reply(0, ERR, 0);
                continue;
            }
        };
        rings::req_ring_read_payload(payload, expect);

        // Diritti per-canale, check subtree (Fase 17): solo le op con path.
        // Gli fd restano capability pure (read/write/close non ricontrollano
        // il path aperto). UTF-8 invalido o spec malformata: passa oltre, lo
        // rifiuta l'handler (i diritti non decidono la validita').
        let subtree_ok = match op_tag {
            R_OPEN | R_MKDIR | R_READDIR | R_DELETE | R_STAT => match core::str::from_utf8(payload) {
                Ok(p) => rights::within_subtree(rights::rights_subtree(&rights, chan), rights::normalize_sub_view(p)),
                Err(_) => true,
            },
            R_MOUNT => match core::str::from_utf8(payload) {
                Ok(spec) => match spec.split_once('\0') {
                    Some((_, target)) => rights::within_subtree(
                        rights::rights_subtree(&rights, chan),
                        rights::normalize_sub_view(target.trim_end_matches('\0')),
                    ),
                    None => true,
                },
                Err(_) => true,
            },
            R_UMOUNT => match core::str::from_utf8(payload) {
                Ok(t) => rights::within_subtree(rights::rights_subtree(&rights, chan), rights::normalize_sub_view(t)),
                Err(_) => true,
            },
            _ => true,
        };
        if !subtree_ok {
            rings::resp_ring_write(ERR, 0, &[]);
            let _ = libr::reply(0, ERR, 0);
            continue;
        }

        // Dispatch in base all'op_tag del ring. Ogni handler riceve gia' il
        // payload estratto: il frame e' stato interamente consumato sopra.
        // Errori tipizzati (Fase 40): gli handler ritornano Result<u64, u64>
        // (Ok = valore, Err = sentinella ERR_*); `to_reply_res` la riversa
        // nel reply IPC e nel response frame.
        let result: Result<u64, u64> = match op_tag {
            R_OPEN => {
                match core::str::from_utf8(&payload) {
                    // w1 del frame R_OPEN = flags (O_CREAT, w0 = len path):
                    // il server li ignorava (creava sempre) — ora POSIX.
                    Ok(path) => handlers::handle_open(&mut fs, &mut ftable, &mut fat_mounts, &mounts, chan, w1, path, &mut fat_gen),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_READ => {
                handlers::handle_read(&mut fs, &mut ftable, &mut pipes, &mut fat_mounts, &rings, chan, w0 as u32, w1 as usize, &mut fat_gen)
            }

            R_WRITE => {
                handlers::handle_write_local(&mut fs, &mut ftable, &mut pipes, &mut fat_mounts, chan, w0 as u32, w1 as usize, &payload, &mut fat_gen)
            }

            R_CLOSE => {
                handlers::handle_close(&mut ftable, &mut pipes, chan, w0 as u32)
            }

            R_READDIR => {
                match core::str::from_utf8(&payload) {
                    Ok("") | Ok("/") => handlers::handle_readdir(&mut fs, &mut fat_mounts, &mounts, &rings, chan, "/", &mut fat_gen),
                    Ok(path) => handlers::handle_readdir(&mut fs, &mut fat_mounts, &mounts, &rings, chan, path, &mut fat_gen),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_MKDIR => {
                match core::str::from_utf8(&payload) {
                    Ok(path) => handlers::handle_mkdir(&mut fs, &fat_mounts, path),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_MOUNT => {
                match core::str::from_utf8(&payload) {
                    Ok(spec) => handlers::handle_mount(&mut fat_mounts, spec, &mut fat_gen),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_UMOUNT => {
                match core::str::from_utf8(&payload) {
                    Ok(target) => handlers::handle_umount(&mut fat_mounts, &ftable, target, &mut fat_gen),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_DELETE => {
                match core::str::from_utf8(&payload) {
                    Ok(path) => handlers::handle_delete(&mut fs, &mut fat_mounts, &mounts, path),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_STAT => {
                match core::str::from_utf8(&payload) {
                    Ok("") | Ok("/") => handlers::handle_stat(&mut fs, &mut fat_mounts, &mounts, &rings, chan, "/", &mut fat_gen),
                    Ok(path) => handlers::handle_stat(&mut fs, &mut fat_mounts, &mounts, &rings, chan, path, &mut fat_gen),
                    Err(_) => Err(ERR_INVALID),
                }
            }

            R_LSEEK => {
                // w0 = fd, w1 = offset (bit reinterpretati come i64),
                // payload[0] = whence (expect = 1 garantisce il byte).
                let off = w1 as i64;
                let whence = payload.first().copied().unwrap_or(0xFF) as u64;
                handlers::handle_lseek(&fs, &mut ftable, &mut fat_mounts, chan, w0 as u32, off, whence, &mut fat_gen)
            }

            R_DUP_GRANT => {
                dup::handle_grant(&ftable, &mut grants, &mut pipes, chan, w0 as u32)
            }

            R_DUP_CLAIM => {
                match payload.first_chunk::<8>() {
                    Some(nonce) => dup::handle_claim(&mut ftable, &mut grants, chan, u64::from_le_bytes(*nonce)),
                    None => Err(ERR_INVALID),
                }
            }

            R_DUP_CANCEL => {
                match payload.first_chunk::<8>() {
                    Some(nonce) => dup::handle_cancel(&mut grants, &mut pipes, chan, u64::from_le_bytes(*nonce)),
                    None => Err(ERR_INVALID),
                }
            }

            // PIPE_CREATE: risposta a due fd (result = lettura, w1 =
            // scrittura), scritta qui perche' il percorso generico in fondo
            // porta un solo valore. `continue`: salta frame+reply generici.
            R_PIPE_CREATE => {
                match handlers::handle_pipe_create(&mut ftable, &mut pipes, chan, w0 as usize) {
                    Ok((r, w)) => {
                        rings::resp_ring_write(r, w, &[]);
                        let _ = libr::reply(0, r, 0);
                    }
                    Err(e) => {
                        let res = mount::to_reply_res(Err(e));
                        rings::resp_ring_write(res, 0, &[]);
                        let _ = libr::reply(0, res, 0);
                    }
                }
                continue;
            }

            // DROP/GET partono dal tetto policy (Fase 45), non da ALL: un
            // canale restrittivo non puo' "droppare verso l'alto".
            R_RIGHTS_DROP => rights::handle_rights_drop(&mut rights, chan, w0 as u32, &payload, ceiling).ok_or(ERR),

            R_RIGHTS_GET => rights::handle_rights_get(&rights, &rings, chan, ceiling).ok_or(ERR),

            _ => {
                // Tag sconosciuto: frame gia' consumato sopra, ritorna errore.
                Err(ERR)
            }
        };

        // Scrivi il response frame (se non e' gia' stato scritto dall'handler).
        // Gli handler locali (read, readdir) scrivono direttamente nella response
        // ring; qui scriviamo solo il result frame per conferma.
        // NOTA: handle_read, handle_readdir, handle_rights_get e handle_stat
        // scrivono payload+result, quindi qui NON dobbiamo scrivere di nuovo.
        // Per gli altri handler, scriviamo solo il result.
        match op_tag {
            R_READ | R_READDIR | R_RIGHTS_GET | R_STAT => {
                // Gli handler locali hanno gia' scritto nella response ring.
                // Per i remote, il driver ha gia' scritto nella response ring.
                // Non fare nulla — il result e' gia' nel frame.
            }
            _ => {
                rings::resp_ring_write(mount::to_reply_res(result), 0, &[]);
            }
        }

        let _ = libr::reply(0, mount::to_reply_res(result), 0);
    }
}
