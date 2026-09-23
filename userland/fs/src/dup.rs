use super::*;

// ── Grant single-use per handoff fd (Fase 40, modello B) ─────────────
// Il parent apre il file, registra qui uno snapshot dell'entry e passa il
// nonce al figlio (via memoria COW pre-fork, mai via IPC: il figlio non ha
// ancora un canale). Il figlio fa CLAIM e ottiene un fd indipendente sul
// PROPRIO canale, con offset copiato (semantica handoff, non condivisione
// POSIX: il parent chiude la sua copia subito dopo il fork).
//
// Attestazione senza token server: il grant salva registrant_pid (da
// `peer_pid` sul canale del grant) e registrant_chan; al claim si verifica
// `ps_info(claimant).parent == registrant_pid` E che il canale del
// registrante sia ancora vivo col pid atteso (`peer_pid` — anti riuso-PID).
// Remote → sempre rifiutato (solo Local si snapshotta).
//
// Single-use: il claim consuma il grant (retry = nuovo grant). Il parent fa
// CANCEL nel cleanup (idempotente). Purge dei grant del registrante su
// EXIT_NOTIFY: mai grant orfani riusabili dopo riuso-PID.

/// Un grant pendente: snapshot dell'entry + chi puo' riscuoterlo.
#[derive(Clone)]
pub struct Grant {
    snap: ftable::FileEntry,
    registrant_pid: u32,
    registrant_chan: u64,
}

/// Tabella nonce → grant (nonce mai 0: 0 = "assente" nei check).
pub struct GrantTable {
    grants: BTreeMap<u64, Grant>,
    next: u64,
}

impl GrantTable {
    pub fn new() -> Self {
        Self { grants: BTreeMap::new(), next: 1 }
    }

    /// Registra un grant, ritorna il nonce (> 0, mai riusato mentre e'
    /// pendente). Il nonce e' contatore monotone mischiato col pid (non un
    /// segreto: l'autorizzazione e' la parentela, verificata al claim — il
    /// nonce e' solo handle anti-collisione tra grant concorrenti).
    pub fn insert(&mut self, g: Grant) -> u64 {
        let mix = (g.registrant_pid as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        loop {
            let mut n = self.next;
            self.next = self.next.wrapping_add(1);
            if n == 0 {
                continue;
            }
            n ^= mix;
            // Mai 0 (riservato) e mai nell'intervallo sentinelle ERR..=ERR_CLOSED
            // (`!0-10..=!0`): un nonce indistinguibile da un rifiuto farebbe
            // fallire il claim sul client (`fs_reply_check` mappa le sentinelle
            // in `Err` prima ancora di usarle come handle).
            if n == 0 || n >= !0u64 - 10 || self.grants.contains_key(&n) {
                continue;
            }
            self.grants.insert(n, g);
            return n;
        }
    }

    pub fn remove(&mut self, nonce: u64) -> Option<Grant> {
        self.grants.remove(&nonce)
    }

    /// Purga i grant di un registrante morto (EXIT_NOTIFY sul suo canale).
    /// Per gli snapshot pipe rilascia anche la reservation (vedi handle_grant:
    /// senza, una pipe con grant orfani non morirebbe mai).
    pub fn purge_registrant(&mut self, chan: u64, pipes: &mut super::pipes::PipeTable) {
        let mut dead: Vec<Grant> = Vec::new();
        self.grants.retain(|_, g| {
            if g.registrant_chan == chan {
                dead.push(Grant {
                    snap: g.snap.clone(),
                    registrant_pid: g.registrant_pid,
                    registrant_chan: g.registrant_chan,
                });
                false
            } else {
                true
            }
        });
        for g in dead.iter() {
            if let ftable::FileEntry::Pipe { pipe, write } = &g.snap {
                pipes.end_closed(*pipe, *write);
            }
        }
    }
}

/// R_DUP_GRANT: w0 = fd locale del chiamante. Snapshot dell'entry + nonce.
/// Ritorna Ok(nonce) o Err(ERR_INVALID) (fd ignoto/remoto, peer ignoto).
/// Fase 42: anche le estremita' pipe si snapshot-tano. Il grant su una pipe
/// PRENOTA subito il conteggio (`end_opened`): la reservation passa al fd del
/// claimant al claim (senza toccare i conteggi) e torna libera al cancel o
/// alla purge. Senza reservation al grant, il close del parent (subito dopo
/// il fork, necessario per propagare l'EOF) correrebbe col claim del figlio
/// e il buffer morirebbe prima di essere riscuotato (race osservata in pipe).
pub fn handle_grant(
    ftable: &ftable::FileTable,
    grants: &mut GrantTable,
    pipes: &mut super::pipes::PipeTable,
    chan: u64,
    fd: u32,
) -> Result<u64, u64> {
    let snap = match ftable.files.get(&(chan, fd)) {
        Some(e @ ftable::FileEntry::Local { .. }) => e.clone(),
        Some(e @ ftable::FileEntry::Pipe { pipe, write }) => {
            if !pipes.end_opened(*pipe, *write) {
                return Err(ERR_INVALID);
            }
            e.clone()
        }
        _ => return Err(ERR_INVALID),
    };
    let pid = libr::peer_pid(chan).map_err(|_| ERR_INVALID)?;
    if pid < 0 || pid > u32::MAX as i64 {
        return Err(ERR_INVALID);
    }
    let pid = pid as u32;
    Ok(grants.insert(Grant { snap, registrant_pid: pid, registrant_chan: chan }))
}

/// R_DUP_CLAIM: payload `[nonce:8]`. Verifica la parentela e alloca un fd
/// indipendente sul canale del claimant. Consuma il grant (single-use).
/// Ritorna Ok(fd) o Err(ERR_INVALID) (nonce ignoto, non-figlio, canale del
/// registrante morto o riusato da altro pid).
/// Fase 42: lo snapshot puo' essere una pipe — la reservation e' gia' stata
/// prenotata al grant, qui si apre solo l'entry (nessun tocco ai conteggi).
pub fn handle_claim(
    ftable: &mut ftable::FileTable,
    grants: &mut GrantTable,
    chan: u64,
    nonce: u64,
) -> Result<u64, u64> {
    if nonce == 0 {
        return Err(ERR_INVALID);
    }
    let g = grants.remove(nonce).ok_or(ERR_INVALID)?;
    // Attestazione doppia: il claimant deve essere figlio del registrante…
    let me = libr::peer_pid(chan).map_err(|_| ERR_INVALID)?;
    if me < 0 || me > u32::MAX as i64 {
        return Err(ERR_INVALID);
    }
    let parent = match libr::ps_info(me as u32) {
        Some(e) => e.parent,
        None => None,
    };
    if parent != Some(g.registrant_pid) {
        return Err(ERR_INVALID);
    }
    // …e il canale del registrante deve essere ancora vivo col pid atteso
    // (il pid potrebbe essere stato riusato dopo la morte del registrante:
    // il grant sarebbe orfano — la purge su EXIT_NOTIFY lo rimuove, ma la
    // race resta chiusa qui per costruzione).
    match libr::peer_pid(g.registrant_chan) {
        Ok(p) if p == g.registrant_pid as i64 => {}
        _ => return Err(ERR_INVALID),
    }
    match &g.snap {
        ftable::FileEntry::Pipe { pipe, write } => {
            Some(ftable.open_pipe(chan, *pipe, *write)).ok_or(ERR_INVALID)
        }
        ftable::FileEntry::Local { .. } => {
            ftable.open_cloned(chan, &g.snap).ok_or(ERR_INVALID)
        }
        _ => Err(ERR_INVALID),
    }
}

/// R_DUP_CANCEL: payload `[nonce:8]`. Best-effort idempotente: sempre Ok,
/// anche a nonce assente (il parent non deve mai wedgiarsi nel cleanup).
/// La reservation pipe del grant cancellato torna libera qui.
pub fn handle_cancel(
    grants: &mut GrantTable,
    pipes: &mut super::pipes::PipeTable,
    _chan: u64,
    nonce: u64,
) -> Result<u64, u64> {
    if let Some(g) = grants.remove(nonce) {
        if let ftable::FileEntry::Pipe { pipe, write } = &g.snap {
            pipes.end_closed(*pipe, *write);
        }
    }
    Ok(0)
}
