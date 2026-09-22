use super::*;
use crate::*;

// ── FS wrappers ───────────────────────────────────────────────────

/// `open(path, flags)`: apre un file tramite il fs server.
/// Ritorna il fd (>=0) o l'errore nativo (Fase 39).
#[inline]
pub fn open(path: &str, flags: u32) -> Result<i64, Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_OPEN, path.len() as u64, flags as u64, path.as_bytes()) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(R_OPEN, path.len() as u64, flags as u64, path.as_bytes())
    }) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|v| v as i64)
        }
        None => Err(Error::NotReady),
    }
}

/// `read_fs(fd, dst, max_count)`: legge fino a `max_count` byte dal file.
/// I dati viaggiano nel response ring; se `max_count` supera la capacita' di
/// un singolo frame, la lettura viene spezzata in piu' round trip (Fase 10.2).
/// Ritorna i byte letti (0 = EOF); un errore dopo progressi parziali ritorna
/// `Ok(got)` (semantica POSIX: conta cio' che c'e'), solo a zero progressi e'
/// `Err` (Fase 39).
pub fn read_fs(fd: i64, dst: &mut [u8], max_count: usize) -> Result<usize, Error> {
    session::fs_gate()?;
    let mut got = 0usize;
    while got < max_count {
        let want = (max_count - got).min(ring::RING_MAX_PAYLOAD);
        if !ring::req_ring_write(R_READ, fd as u64, want as u64, &[]) {
            return if got > 0 { Ok(got) } else { Err(Error::RingFull) };
        }
        let n = match session::fs_notify_result(FS_NOTIFY, || {
            ring::req_ring_write(R_READ, fd as u64, want as u64, &[])
        }) {
            Some((result, _, payload_len)) => {
                if session::fs_reply_check(result).is_err() {
                    ring::resp_ring_consume(16);
                    if got > 0 {
                        return Ok(got);
                    }
                    return Err(Error::Failed);
                }
                let avail = (result as usize).min(payload_len).min(max_count - got);
                if avail > 0 {
                    ring::resp_ring_read_payload(&mut dst[got..got + avail], avail);
                } else {
                    ring::resp_ring_consume(16);
                }
                let _ = result;
                avail
            }
            None => {
                if got > 0 {
                    return Ok(got);
                }
                return Err(Error::NotReady);
            }
        };
        if n == 0 {
            break; // EOF
        }
        got += n;
        if n < want {
            break; // read corto (EOF o file piu' corto)
        }
    }
    Ok(got)
}

/// `write_fs(fd, src, count)`: scrive `count` byte sul file (dal request ring).
/// I dati viaggiano nel request ring; se `count` supera la capacita' di un
/// singolo frame, la scrittura viene spezzata in piu' round trip (Fase 10.2).
/// Ritorna i byte scritti; errore dopo progressi parziali = `Ok(done)` (come
/// `read_fs`, Fase 39).
pub fn write_fs(fd: i64, src: &[u8], count: usize) -> Result<usize, Error> {
    session::fs_gate()?;
    let mut done = 0usize;
    while done < count {
        let want = (count - done).min(ring::RING_MAX_PAYLOAD);
        if !ring::req_ring_write(R_WRITE, fd as u64, want as u64, &src[done..done + want]) {
            return if done > 0 { Ok(done) } else { Err(Error::RingFull) };
        }
        let n = match session::fs_notify_result(FS_NOTIFY, || {
            ring::req_ring_write(R_WRITE, fd as u64, want as u64, &src[done..done + want])
        }) {
            Some((result, _, _)) => {
                ring::resp_ring_consume(16);
                let r = match session::fs_reply_check(result) {
                    Ok(v) => v,
                    Err(e) => {
                        if done > 0 {
                            return Ok(done);
                        }
                        return Err(e);
                    }
                };
                (r as usize).min(want)
            }
            None => {
                if done > 0 {
                    return Ok(done);
                }
                return Err(Error::NotReady);
            }
        };
        if n == 0 {
            break;
        }
        done += n;
        if n < want {
            break;
        }
    }
    Ok(done)
}

/// `close(fd)`: chiude un file descriptor.
#[inline]
pub fn close(fd: i64) -> Result<(), Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_CLOSE, fd as u64, 0, &[]) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || ring::req_ring_write(R_CLOSE, fd as u64, 0, &[])) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// `readdir(path, entries_buf, buf_len)`: legge le entry di una directory.
/// Le entry vengono scritte dal server nel response ring nel formato
/// "name\0name\0...\0\0"; le copiamo in `entries_buf`. Ritorna il numero di
/// entry (Fase 39).
#[inline]
pub fn readdir(path: &str, entries_buf: &mut [u8], buf_len: usize) -> Result<usize, Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_READDIR, path.len() as u64, 0, path.as_bytes()) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(R_READDIR, path.len() as u64, 0, path.as_bytes())
    }) {
        Some((result, _, payload_len)) => {
            // Disciplina ring (Fase 17): il response frame si consuma SEMPRE
            // (anche a diniego), POI si interpreta — mai early-return prima.
            let count = session::fs_reply_check(result);
            if count.is_ok() && payload_len > 0 {
                ring::resp_ring_read_payload(entries_buf, payload_len.min(buf_len));
            } else {
                ring::resp_ring_consume(16);
            }
            count.map(|c| c as usize)
        }
        None => Err(Error::NotReady),
    }
}

/// `mkdir(path)`: crea una directory tramite il fs server (Fase 39).
#[inline]
pub fn mkdir(path: &str) -> Result<(), Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_MKDIR, path.len() as u64, 0, path.as_bytes()) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(R_MKDIR, path.len() as u64, 0, path.as_bytes())
    }) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// `remove(path)`: cancella un file o una directory VUOTA (Fase 18.2).
/// Solo ramfs: FAT read-only e device remoti rifiutano (Fase 39).
#[inline]
pub fn remove(path: &str) -> Result<(), Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_DELETE, path.len() as u64, 0, path.as_bytes()) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(R_DELETE, path.len() as u64, 0, path.as_bytes())
    }) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// Fase 19.2 — metadati di un path (zero kernel: frame R_STAT a userfs, nessun
/// fd coinvolto). `size` = byte del file (0 per dir/device); `kind` = tipo
/// (STAT_FILE/DIR/DEVICE); `readonly` = bit 7 (FAT sempre, ramfs mai, device
/// mai affermato senza interrogare il driver).
#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub size: u64,
    pub kind: u64,
    pub readonly: bool,
}

impl Stat {
    pub fn is_file(&self) -> bool {
        self.kind & 0x3 == STAT_FILE
    }
    pub fn is_dir(&self) -> bool {
        self.kind & 0x3 == STAT_DIR
    }
    pub fn is_device(&self) -> bool {
        self.kind & 0x3 == STAT_DEVICE
    }
}

/// `stat(path, out)`: metadati senza aprire (Fase 39; `NotFound` tipizzato in
/// Fase 40, oggi `Failed`).
#[inline]
pub fn stat(path: &str, out: &mut Stat) -> Result<(), Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_STAT, path.len() as u64, 0, path.as_bytes()) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(R_STAT, path.len() as u64, 0, path.as_bytes())
    }) {
        // Risposta self-written `[size:8][kind:8]`: result=size, w1=kind.
        Some((result, w1, _)) => {
            ring::resp_ring_consume(16);
            let size = session::fs_reply_check(result)?;
            out.size = size;
            out.kind = w1 & 0x3;
            out.readonly = w1 & STAT_READONLY != 0;
            Ok(())
        }
        None => Err(Error::NotReady),
    }
}

/// Scrive un frame "source\0target\0" e lo notifica (helper di `mount`).
fn mount_frame(source: &str, target: &str) -> bool {
    // Path lunghi al massimo MAX_PATH (256) l'uno + 2 NUL.
    let total = source.len() + 1 + target.len() + 1;
    if total > ring::RING_MAX_PAYLOAD || total > 514 {
        return false;
    }
    let mut buf = [0u8; 520];
    buf[..source.len()].copy_from_slice(source.as_bytes());
    buf[source.len()] = 0;
    buf[source.len() + 1..source.len() + 1 + target.len()].copy_from_slice(target.as_bytes());
    buf[source.len() + 1 + target.len()] = 0;
    ring::req_ring_write(R_MOUNT, total as u64, 0, &buf[..total])
}

/// `mount(source, target)`: monta una sorgente a blocchi (es. "/dev/sda")
/// su un target (es. "/mnt", Fase 16b). Errori nativi in Fase 39 (dettaglio
/// in Fase 40).
#[inline]
pub fn mount(source: &str, target: &str) -> Result<(), Error> {
    session::fs_gate()?;
    if !mount_frame(source, target) {
        return Err(Error::Invalid);
    }
    match session::fs_notify_result(FS_NOTIFY, || mount_frame(source, target)) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// `umount(target)`: smonta un target (Fase 16b). Rifiutato se ci sono fd
/// aperti sotto il target (Fase 39; `Busy` tipizzato in Fase 40).
#[inline]
pub fn umount(target: &str) -> Result<(), Error> {
    session::fs_gate()?;
    if !ring::req_ring_write(R_UMOUNT, target.len() as u64, 0, target.as_bytes()) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(R_UMOUNT, target.len() as u64, 0, target.as_bytes())
    }) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// `rights_drop(keep_mask, subtree)`: riduce i propri diritti sul canale
/// verso userfs (Fase 17, self-restriction only). Solo shrink: il server fa
/// AND con la mask corrente; il subtree puo' solo restringersi (widen =
/// `Err`, nessun cambio). `subtree=None` = solo-ops (Fase 39).
/// Irrevocabile per disegno (nessun GRANT: i canali non sono trasferibili).
#[inline]
pub fn rights_drop(keep_mask: u32, subtree: Option<&str>) -> Result<(), Error> {
    session::fs_gate()?;
    let sub_bytes: &[u8] = match subtree {
        Some(s) => s.as_bytes(),
        None => &[],
    };
    if sub_bytes.len() > 256 {
        return Err(Error::Invalid);
    }
    if !ring::req_ring_write(
        R_RIGHTS_DROP,
        keep_mask as u64,
        sub_bytes.len() as u64,
        sub_bytes,
    ) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || {
        ring::req_ring_write(
            R_RIGHTS_DROP,
            keep_mask as u64,
            sub_bytes.len() as u64,
            sub_bytes,
        )
    }) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// `rights_get(buf)`: legge i propri diritti (Fase 17). Scrive il subtree
/// normalizzato + NUL in `buf` (root = solo NUL) e ritorna la mask ops
/// (0..=RIGHTS_ALL). Dimensionare `buf` ≥ 257 (Fase 39).
#[inline]
pub fn rights_get(buf: &mut [u8]) -> Result<u32, Error> {
    if buf.is_empty() {
        return Err(Error::Invalid);
    }
    session::fs_gate()?;
    if !ring::req_ring_write(R_RIGHTS_GET, 0, 0, &[]) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_NOTIFY, || ring::req_ring_write(R_RIGHTS_GET, 0, 0, &[])) {
        Some((result, w1, payload_len)) => {
            // Stessa disciplina di `readdir`: consuma sempre, poi interpreta.
            let ops = session::fs_reply_check(result);
            // Leggi tutto il payload in uno stack buffer (subtree ≤ 256 dal
            // server): un solo consumo 16+len, mai disallineamenti.
            let mut tmp = [0u8; 256];
            let take = payload_len.min(256);
            if take > 0 {
                ring::resp_ring_read_payload(&mut tmp, take);
            } else {
                ring::resp_ring_consume(16);
            }
            let ops = ops?;
            let n = (w1 as usize).min(take).min(buf.len() - 1);
            buf[..n].copy_from_slice(&tmp[..n]);
            buf[n] = 0;
            Ok(ops as u32)
        }
        None => Err(Error::NotReady),
    }
}

/// Identità stabile di un volume FAT32 dal boot sector (Fase 16d):
/// `(seriale, label_raw_11B)`. Seriale solo con firma estesa `0x29` (layout
/// standard firma-a-66/volid-67-70, o variante mkfat firma-a-67/volid-68-71);
/// label sempre (11 byte raw, trim a carico del chiamante). `None` se il
/// settore non e' un BPB FAT valido (stessi check minimi di mount: 55AA,
/// bps 512, spc potenza di 2 non zero, almeno una FAT non vuota, root ≥ 2).
/// Usato sia dal parser (`userfs/fat32.rs`) che dallo sniff per-nodo del
/// driver (`userdisk`): un nodo annuncia UUID/label sse monta davvero.
pub fn fat_bpb_identity(boot: &[u8; 512]) -> Option<(Option<u32>, [u8; 11])> {
    if boot[510] != 0x55 || boot[511] != 0xAA {
        return None;
    }
    let bps = u16::from_le_bytes([boot[11], boot[12]]);
    let spc = boot[13];
    let num_fats = boot[16];
    let fat_size = u32::from_le_bytes([boot[36], boot[37], boot[38], boot[39]]);
    let root = u32::from_le_bytes([boot[44], boot[45], boot[46], boot[47]]);
    if bps != 512 || spc == 0 || (spc & (spc - 1)) != 0 {
        return None;
    }
    if num_fats == 0 || fat_size == 0 || root < 2 {
        return None;
    }
    let vol_serial = if boot[66] == 0x29 {
        Some(u32::from_le_bytes([boot[67], boot[68], boot[69], boot[70]]))
    } else if boot[67] == 0x29 {
        Some(u32::from_le_bytes([boot[68], boot[69], boot[70], boot[71]]))
    } else {
        None
    };
    let mut vol_label = [0u8; 11];
    vol_label.copy_from_slice(&boot[71..82]);
    Some((vol_serial, vol_label))
}

/// `fs_register(prefix)`: un driver (devfs/console) registra il proprio prefix
/// di mount presso userfs (Fase 39: errore nativo; il chiamante puo' ritentare
/// se userfs non e' ancora pronto).
#[inline]
pub fn fs_register(prefix: &[u8]) -> Result<(), Error> {
    fs_register_multi(&[prefix])
}

/// `fs_register_multi(prefixes)`: registra PIU' prefix con UNA SOLA IPC
/// sincrona (Fase 16d). Serve ai driver multi-nodo (devfs: `/dev/null` +
/// `/dev/zero`): due register sincroni consecutivi creerebbero un mount
/// forwardable dopo il primo, e se userfs in quel momento sta inoltrando una
/// richiesta al driver (single-threaded, `send` bloccante) si crea un
/// deadlock incrociato (driver→userfs register, userfs→driver forward).
/// Payload = prefix separati da NUL. `Ok` se TUTTI registrati (Fase 39).
pub fn fs_register_multi(prefixes: &[&[u8]]) -> Result<(), Error> {
    session::fs_gate()?;
    let mut buf = [0u8; 520];
    let mut n = 0usize;
    for (i, p) in prefixes.iter().enumerate() {
        if i > 0 {
            if n + 1 > buf.len() {
                return Err(Error::Invalid);
            }
            buf[n] = 0;
            n += 1;
        }
        if n + p.len() > buf.len() {
            return Err(Error::Invalid);
        }
        buf[n..n + p.len()].copy_from_slice(p);
        n += p.len();
    }
    if n == 0 {
        return Err(Error::Invalid);
    }
    if !ring::req_ring_write(R_REGISTER, n as u64, 0, &buf[..n]) {
        return Err(Error::RingFull);
    }
    match session::fs_notify_result(FS_REGISTER, || {
        ring::req_ring_write(R_REGISTER, n as u64, 0, &buf[..n])
    }) {
        Some((result, _, _)) => {
            ring::resp_ring_consume(16);
            session::fs_reply_check(result).map(|_| ())
        }
        None => Err(Error::NotReady),
    }
}

/// Legge un file intero in heap (bound 256 KiB = SPAWN_IMAGE_MAX kernel).
/// None su qualunque errore (open/read/close) o file vuoto. Chunk da
/// RING_MAX_PAYLOAD: un round-trip per chunk (ogni round-trip puo' attendere
/// un quanto sotto carico: dimezzarli dimezza il tempo di load).
/// (A4: prima identico in init/usertests/usertest-client come
/// `load_file`/`load_bin`; la variante init accettava anche il file vuoto,
/// qui rifiutato — un .bin vuoto non e' mai valido e falliva loud comunque).
pub fn load_file(path: &str) -> Option<alloc::vec::Vec<u8>> {
    let fd = open(path, 0).ok()?;
    let mut data = alloc::vec::Vec::new();
    let mut chunk = [0u8; ring::RING_MAX_PAYLOAD];
    loop {
        if data.len() >= 256 * 1024 {
            let _ = close(fd);
            return None; // troppo grosso: mai un binario valido
        }
        let n = match read_fs(fd, &mut chunk, ring::RING_MAX_PAYLOAD) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
    }
    let _ = close(fd);
    if data.is_empty() {
        return None;
    }
    Some(data)
}
