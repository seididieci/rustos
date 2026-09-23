use super::*;

// ── Pipe buffer server-side (Fase 42) ─────────────────────────────────
// Una pipe e' un buffer byte condiviso tra due fd (lettura + scrittura),
// identificato da un id opaco. Semantica STRETTAMENTE non-bloccante: il
// server e' single-threaded event-driven (un recv → una reply), quindi non
// puo' mai dormire in attesa di dati/spazio — il client riprova throttled
// (vedi Error::Empty/Closed in libr). Regole:
// - read a vuota con writer aperti → Empty (riprova, MAI wedge);
// - read a vuota con writer chiusi → 0 (EOF vero);
// - write oltre la capacita' → parziale (come i file, il client rimanda);
// - write senza lettori → Closed (equivalente SIGPIPE senza segnali);
// - l'ultima close (entrambe le estremita' a zero) libera il buffer.
// Mai heap per-op oltre il buffer stesso (creato una volta a PIPE_CREATE,
// drenato per chunk ≤ 4096 come i file; regola Fase 24).

/// Capacita' di default del buffer (2 round-trip di chunk pieno).
pub const PIPE_DEFAULT_CAP: usize = 8192;
/// Bound dell'hint di R_PIPE_CREATE (w0): sotto = default, sopra = clamp.
pub const PIPE_MIN_CAP: usize = 4096;
pub const PIPE_MAX_CAP: usize = 16384;

struct PipeBuf {
    data: alloc::collections::VecDeque<u8>,
    cap: usize,
    r_open: u32,
    w_open: u32,
}

pub struct PipeTable {
    pipes: BTreeMap<u32, PipeBuf>,
    next: u32,
}

impl PipeTable {
    pub fn new() -> Self {
        Self { pipes: BTreeMap::new(), next: 1 }
    }

    /// Crea una pipe con (r_open, w_open) = (1, 1). Ritorna l'id (> 0).
    pub fn create(&mut self, hint: usize) -> u32 {
        let cap = if hint < PIPE_MIN_CAP {
            PIPE_DEFAULT_CAP
        } else {
            hint.min(PIPE_MAX_CAP)
        };
        loop {
            let id = self.next;
            self.next = self.next.wrapping_add(1).max(1);
            if id != 0 && !self.pipes.contains_key(&id) {
                self.pipes.insert(id, PipeBuf {
                    data: alloc::collections::VecDeque::new(),
                    cap,
                    r_open: 1,
                    w_open: 1,
                });
                return id;
            }
        }
    }

    /// Un'altra estremita' si apre sullo stesso buffer (claim di un grant):
    /// incrementa il contatore del lato giusto. False a id ignoto.
    pub fn end_opened(&mut self, id: u32, write: bool) -> bool {
        match self.pipes.get_mut(&id) {
            Some(p) => {
                if write {
                    p.w_open += 1;
                } else {
                    p.r_open += 1;
                }
                true
            }
            None => false,
        }
    }

    /// Un'estremita' si chiude (close o purge del canale). A entrambe a zero
    /// il buffer viene liberato (niente leak a pipeline finite).
    pub fn end_closed(&mut self, id: u32, write: bool) {
        let dead = match self.pipes.get_mut(&id) {
            Some(p) => {
                if write {
                    p.w_open = p.w_open.saturating_sub(1);
                } else {
                    p.r_open = p.r_open.saturating_sub(1);
                }
                p.r_open == 0 && p.w_open == 0
            }
            None => false,
        };
        if dead {
            self.pipes.remove(&id);
        }
    }

    /// Legge fino a `out.len()` byte. Ritorna (n, eof): n > 0 = dati; n == 0
    /// con eof = writer chiusi (EOF vero); n == 0 senza eof = vuota ma writer
    /// aperti (il chiamante risponde ERR_EMPTY, mai 0 che sembrerebbe EOF).
    pub fn read(&mut self, id: u32, out: &mut [u8]) -> Option<(usize, bool)> {
        let p = self.pipes.get_mut(&id)?;
        let mut n = 0;
        while n < out.len() {
            match p.data.pop_front() {
                Some(b) => {
                    out[n] = b;
                    n += 1;
                }
                None => break,
            }
        }
        Some((n, p.w_open == 0))
    }

    /// Scrive fino a `cap - len` byte. Ritorna i byte accettati (0 = piena;
    /// il chiamante segnala parziale, mai errore). None a id ignoto.
    pub fn write(&mut self, id: u32, data: &[u8]) -> Option<usize> {
        let p = self.pipes.get_mut(&id)?;
        if p.r_open == 0 {
            return None; // nessun lettore: il chiamante risponde ERR_CLOSED
        }
        let mut n = 0;
        while n < data.len() && p.data.len() < p.cap {
            p.data.push_back(data[n]);
            n += 1;
        }
        Some(n)
    }

    /// True se la pipe ha ancora lettori (il writer decide tra parziale e stop).
    pub fn has_readers(&self, id: u32) -> bool {
        self.pipes.get(&id).map_or(false, |p| p.r_open > 0)
    }
}
