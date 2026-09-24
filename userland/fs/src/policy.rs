use super::*;

// ── Policy su identita' (Fase 45, sandbox build) ───────────────────
// La self-restriction (Fase 17) non vincola chi non droppa: qui userfs
// applica un TETTO server-side per hash misurato. Al primo handshake
// (FS_BUF_REG) il canale viene classificato UNA volta (peer_info = hash
// FNV-1a misurato dal kernel allo spawn, ADR-0027) e il tetto resta in cache
// fino all'EXIT_NOTIFY (stessa vita di rings/rights: niente TOCTOU oltre la
// purga esistente, niente syscall per-op nel choke point).
//
// Effettivo nel choke point: `drop_mask & ceiling` (la policy non allarga
// mai: un DROP resta irrevocabile, t34 invariato).
//
// Classificazione (ordine):
// 1. pid 1 (init): ALL. init e' escluso dal manifest (ciclo hash-di-se',
//    Fase 36) ma e' TCB spawnato dal kernel (mai riusato: la sua morte e'
//    panic). Senza, init non caricherebbe i binari da /fat.
// 2. hash noto al manifest servizi (`SERVICE_POLICY`, generato a build):
//    mask della riga (TCB = ALL, programmi di terzi = restrittiva).
// 3. hash noto ai test (`TEST_POLICY`, solo build test): ALL (la suite
//    esercita ogni op; i negativi stanno in t57 sul default).
// 4. ignoto: DEFAULT_UNKNOWN_OPS (niente MOUNT/UMOUNT/GRANT/PIPE — le op
//    che creano stato globale o capability per altri; file e dir restano
//    usabili). Fail-closed: a peer_info fallito, stesso default.
//
// La tabella test e' un file SEPARATO incluso SOLO da userfs: i binari test
// incorporano service_hashes (t51) ma NON test_policy, quindi niente ciclo
// (stesso motivo dell'esclusione userinit/userfs in Fase 36).

/// Default fail-closed per hash ignoto (Fase 45): ALL senza MOUNT (0x20),
/// UMOUNT (0x40), GRANT (0x200), PIPE (0x400) = 0x19F. OPEN/READ/WRITE/
/// READDIR/MKDIR/DELETE/SEEK restano: un programma ignoto legge/scrive file
/// ma non monta, non crea pipe, non concede fd.
pub const DEFAULT_UNKNOWN_OPS: u32 = 0x19F;

/// Tetto ops per il canale `chan` (chiamato una volta all'handshake).
pub fn ceiling_for(chan: u64) -> u32 {
    // (1) init: TCB pid 1, fuori manifest per costruzione.
    if libr::peer_pid(chan).unwrap_or(-1) == 1 {
        return libr::RIGHTS_ALL;
    }
    // (2+3) hash noto: cerca prima nei servizi, poi nei test.
    if let Ok(h) = libr::peer_info(chan) {
        for &(ph, mask) in SERVICE_POLICY {
            if ph == h {
                return mask;
            }
        }
        for &(ph, mask) in TEST_POLICY {
            if ph == h {
                return mask;
            }
        }
    }
    // (4) ignoto o canale morente: fail-closed.
    DEFAULT_UNKNOWN_OPS
}
