use super::*;

// ── Fase 39 (P0, fondamenta posix) + 40.3 (skeleton supervisionato) ───
// t53: (a) Posix registrato e supervisionato: lookup riesce, pid noto e
// figlio di init (lineage di supervisione, mai squat); (b) tabella to_errno
// totale e fissata; (c) gate di registrazione sul nuovo slot 8 via helper
// non-figlio-di-init (stesso probe di t50, che resta intatto: qui si esercita
// service_from_disc(8) + braccio nome "posix").
//
// Nota storica: in Fase 39 (a) asseriva lookup/pid = NotFound (nessun server).
// Dalla 40.3 il skeleton gira supervisionato: l'assenza sarebbe un FAIL.

/// t53 — fondamenta posix: registry, errore nativo, gate.
pub fn t_posix_foundation() -> bool {
    helpers::drain_stray();
    // (a) Server Posix su e supervisionato: lookup riesce subito (niente
    // bound da attendere: init lo spawna prima della suite) e il pid e'
    // figlio di init (stessa lineage degli altri servizi).
    let _ = match libr::service_lookup(libr::Service::Posix) {
        Ok(chan) => chan,
        Err(e) => {
            println!("[usertests] t53: lookup Posix = Err({:?}) (atteso Ok)", e);
            return false;
        }
    };
    let pid = match libr::service_pid(libr::Service::Posix) {
        Ok(p) => p,
        Err(e) => {
            println!("[usertests] t53: service_pid Posix = Err({:?}) (atteso Ok)", e);
            return false;
        }
    };
    match libr::ps_info(pid as u32) {
        Some(e) if e.parent == Some(1) => {}
        other => {
            println!("[usertests] t53: posix pid={} parent illegittimo: {:?}", pid, other.map(|e| e.parent));
            return false;
        }
    }
    // (b) UNICA traduzione nativo→errno: tabella totale e fissata. Se una
    // variante futura nasce senza braccio qui, non compila (match totale).
    let table: [(libr::Error, i64); 15] = [
        (libr::Error::NotReady, libr::posix::EIO),
        (libr::Error::Pending, libr::posix::EAGAIN),
        (libr::Error::RingFull, libr::posix::EAGAIN),
        (libr::Error::ServerDied, libr::posix::EIO),
        (libr::Error::Denied, libr::posix::EACCES),
        (libr::Error::NoMemory, libr::posix::ENOMEM),
        (libr::Error::Busy, libr::posix::EBUSY),
        (libr::Error::Invalid, libr::posix::EINVAL),
        (libr::Error::Failed, libr::posix::EIO),
        (libr::Error::NotFound, libr::posix::ENOENT),
        (libr::Error::NotDir, libr::posix::ENOTDIR),
        (libr::Error::IsDir, libr::posix::EISDIR),
        (libr::Error::Exists, libr::posix::EEXIST),
        (libr::Error::ReadOnly, libr::posix::EROFS),
        (libr::Error::TooBig, libr::posix::EFBIG),
    ];
    for (e, want) in table {
        let got = libr::posix::to_errno(e);
        if got != want {
            println!("[usertests] t53: to_errno({:?}) = {} (atteso {})", e, got, want);
            return false;
        }
    }
    // (c) Gate sul nuovo slot: helper non-figlio-di-init prova kill ostile +
    // register Init + register Posix — tutti e tre rifiutati (ok=true).
    let (b_chan, b_pid) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_KILLME, 0,
    ) {
        Some(x) => x,
        None => {
            println!("[usertests] t53: spawn vittima FAILED");
            return false;
        }
    };
    let (h_chan, _) = match helpers::spawn_cfg(
        "/fat/test/testcli.bin", "utcli", 16, helpers::M_HARDEN, b_pid,
    ) {
        Some(x) => x,
        None => {
            let _ = libr::kill(b_pid as i64, 0);
            let _ = helpers::wait_exit(b_chan);
            println!("[usertests] t53: spawn harden FAILED");
            return false;
        }
    };
    let (ok, detail) = helpers::recv_done(&[h_chan]);
    let victim_alive = libr::ps_info(b_pid as u32).is_some();
    let _ = libr::kill(b_pid as i64, 0);
    let _ = helpers::wait_exit(b_chan);
    if !ok || !victim_alive {
        println!("[usertests] t53: harden FAIL (ok={}, detail={}, victim_alive={})", ok, detail, victim_alive);
        return false;
    }
    true
}
