use super::*;

// ── Mount (registrazione FS puramente async) ────────────────────────
// Vedi doc in testa: MAI send sincrone verso userfs. Ring FS propri (allocati
// raw, mappati qui, mai iniettati da nessuno) + FS_BUF_REG / R_REGISTER via
// send_async + collect per req_id. Una sola op FS in volo (come libr).

/// Stato della registrazione FS: handshake poi un prefix alla volta.
/// SENZA throttle: ogni tentativo fallito si riprova al prossimo wakeup (i
/// tentativi sono solo lookup/send cheap e `recv` blocca sempre dopo — mai
/// spin). Lo sleep in `recv` senza waker congelerebbe i retry (osservato:
/// registrazione ferma per sempre dopo un lookup fallito a boot).
pub(crate) struct FsReg {
    /// Fisici dei ring FS propri (per FS_BUF_REG).
    fs_req_phys: u64,
    fs_resp_phys: u64,
    /// Canale verso userfs (None = da risolvere).
    chan: Option<u64>,
    /// Handshake FS_BUF_REG completato sul canale corrente.
    bufreg_done: bool,
    /// req_id dell'op FS in volo (None = libero).
    pending: Option<i64>,
    /// Prossimo nodo da registrare.
    idx: usize,
}

impl FsReg {
    pub(crate) fn new(fs_req_phys: u64, fs_resp_phys: u64) -> Self {
        Self {
            fs_req_phys,
            fs_resp_phys,
            chan: None,
            bufreg_done: false,
            pending: None,
            idx: 0,
        }
    }

    /// Reset dopo morte di userfs (EXIT_NOTIFY): mounts purgati di la', i ring
    /// resettati di qua', si ricomincia da handshake + primo nodo.
    pub(crate) fn reset(&mut self) {
        libr::println!("[userdisk] reset registrazione FS (userfs morto)");
        self.chan = None;
        self.bufreg_done = false;
        self.pending = None;
        self.idx = 0;
        rings::fs_rings_reset();
    }

    /// Completa se tutti i prefix registrati.
    fn done(&self, total: usize) -> bool {
        self.idx >= total
    }

    /// Avanza di UN passo (mai bloccante): risolve, handshake, registra.
    /// INVARIANTE (lezione tty): l'invio avviene NELLA STESSA chiamata che
    /// entra nella fase — un giro chiuso in recv senza aver inviato dorme.
    /// Ritenta a OGNI wakeup senza throttle: i tentativi sono solo lookup e
    /// send cheap, e `recv` blocca sempre dopo (mai spin). Uno sleep con
    /// throttle e senza waker congelerebbe i retry per sempre.
    pub(crate) fn step(&mut self, prefixes: &[String]) {
        if self.done(prefixes.len()) || self.pending.is_some() {
            return;
        }
        // Canale (re-lookup se assente/stale: la send_async fallita lo azzera).
        let chan = match self.chan {
            Some(c) => c,
            None => match libr::service_lookup(libr::Service::Fs) {
                Ok(c) => {
                    self.chan = Some(c as u64);
                    self.bufreg_done = false;
                    c as u64
                }
                Err(_) => {
                    return;
                }
            },
        };
        if !self.bufreg_done {
            match libr::send_async(chan, FS_BUF_REG, self.fs_req_phys, self.fs_resp_phys) {
                Ok(req) => {
                    self.pending = Some(req);
                }
                Err(_) => {
                    self.chan = None;
                }
            }
            return;
        }
        // Un prefix alla volta (frame + notify async): nodi `/dev/sdX` e
        // alias stabili `/dev/disk/by-uuid/*`, `/dev/disk/by-label/*`.
        let prefix = &prefixes[self.idx];
        let bytes = prefix.as_bytes();
        if !rings::fs_req_write(R_REGISTER, bytes.len() as u64, 0, bytes) {
            return;
        }
        match libr::send_async(chan, FS_REGISTER, 0, 0) {
            Ok(req) => {
                self.pending = Some(req);
            }
            Err(_) => {
                rings::fs_req_rollback(20 + bytes.len());
                self.chan = None;
            }
        }
    }

    /// Raccoglie una reply async che matcha il pending. Ritorna true se era
    /// nostra (consumata), con avanzamento di stato.
    pub(crate) fn collect_if_mine(&mut self, req_id: i64, prefixes: &[String]) -> bool {
        let pending = match self.pending {
            Some(p) if req_id > 0 && req_id == p => p,
            _ => return false,
        };
        let _ = pending;
        // BUF_REG non ha frame (register-only): basta il match.
        if !self.bufreg_done {
            self.bufreg_done = true;
            self.pending = None;
            return true;
        }
        // REGISTER: result dal response frame (0 = registrato).
        match rings::fs_resp_read() {
            Some(0) => {
                self.pending = None;
                libr::println!("[userdisk] registered {} with userfs", prefixes[self.idx]);
                self.idx += 1;
            }
            _ => {
                // userfs ha scartato il frame (resync) o ring vuoto: pending
                // libero, si riprova al prossimo wakeup (mai throttle senza
                // waker: vedi `step`).
                self.pending = None;
            }
        }
        true
    }
}
