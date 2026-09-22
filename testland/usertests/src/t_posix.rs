use super::*;

// ── Fase 39 (P0, fondamenta posix) ─────────────────────────────────────
// t53: (a) lookup/pid di Posix pre-server = NotFound pulito (nessun hang);
// (b) tabella to_errno totale e fissata; (c) gate di registrazione sul nuovo
// slot 8 via helper non-figlio-di-init (stesso probe di t50, che resta
// intatto: qui si esercita service_from_disc(8) + braccio nome "posix").

/// t53 — fondamenta posix: registry, errore nativo, gate.
pub fn t_posix_foundation() -> bool {
    helpers::drain_stray();
    // (a) Nessun server Posix registrato: lookup e pid falliscono puliti.
    // Il lookup ha bound interno (ritenta a boot); pre-server deve dare
    // NotFound, mai hang: il servizio non esiste e non esistera' in Fase 39.
    match libr::service_lookup(libr::Service::Posix) {
        Err(libr::Error::NotFound) => {}
        other => {
            println!("[usertests] t53: lookup Posix = {:?} (atteso Err(NotFound))", other);
            return false;
        }
    }
    if libr::service_pid(libr::Service::Posix) != Err(libr::Error::NotFound) {
        println!("[usertests] t53: service_pid Posix non NotFound");
        return false;
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
