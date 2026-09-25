# ArcaFS — specifica (bozza A0)

Filesystem nativo non-POSIX per Velordor: object store versionato con COW,
snapshot, quota, ACL/ABAC. POSIX solo come vista (mapping sintetico).
Filosofia ADR-0025: nativo dentro (userfs), personalita' al bordo (libr).

Stato: sessione guidata A0 completata (decisioni T0–T10). Prossimo: stesura
di dettaglio punto per punto, poi A1 (singolo-device locale).

> Nota sui gate: i numeri citati altrove sono snapshot storici; il gate
> corrente vive in `docs/src/11-testing.md` e in `ROADMAP.md`.

## 0. Vision e principi

- Non-POSIX nel profondo (`open/read/write` non sono la fondazione),
  integrato con l'OS: `libr` e binari di sistema parlano nativo ArcaFS.
- Vincoli software mai sacri (ring, `DISK_*`, single-thread riscrivibili
  quando un topic lo richiede); vincoli hardware rispettati (settore 512B,
  seek HDD, RAM finita per le cache).
- Ogni struttura a cardinalita' futura ignota: **inline + overflow**
  (mai tabelle a dimensione fissa che diventano muri).
- Dimensioni negoziate nel superblock, mai costanti cablate (`block_size`,
  soglie S1/S2, cap transient set).

## 1. Modello dati (T1)

- Namespace piatto: `bucket` + `chiave` opaca. `/` solo convenzione di
  listing (pattern S3); la gerarchia Unix e' composizione di mount, non
  struttura del FS.
- Blob **versionati**: ogni put crea una versione, le vecchie restano per
  snapshot/GC. Put su chiave esistente = nuova versione (mai errore, mai
  sovrascrittura logica).
- Identita': `object_id: u64` monotonico per volume, immutabile, mai riusato
  (disciplina F2: niente ABA). La chiave e' rinominabile, l'UUID no.
- Unicita' globale: `(volume_uuid, object_id)` — niente UUID-128, niente
  collisioni al merge (A8).
- Chiavi relative (mount+bucket strippati); soglia inline ≈ 200B: i path
  realistici restano inline, l'overflow e' per chiavi patologiche.
- Bucket multipli come mount: `/`, `/home`, `/home/user` (annidato),
  `/var`, … — un bucket = un subvolume = un mount (quota/policy/snapshot
  seguono la granularita' del mount).

## 2. Indicizzazione (T2)

- **Primary B+tree per UUID** → `{versioni, quota, attributi}`: snapshot,
  clone, GC e quota parlano UUID, mai nomi (POSIX e' solo flavour).
- **Secondary per `(bucket,key)`** → UUID + stat denormalizzata
  (size, mtime, version_head): listing in un range scan, `stat` senza
  toccare il primary.
- Nodi da **3584 B** (`block_size` negoziato, fisso v1) = 1 chunk `DISK_*`
  esatto; FNV-1a/64 + confronto byte esatto (mai solo-hash).
- Foglie con overflow per chiavi lunghe; discesa inalterata (separatori
  corti), +1 read solo alla conferma.
- Rename stesso volume = delete+insert solo sul secondary (atomico,
  zero copie); cross-subvolume = cp + tombstone (non atomico, dichiarato).
- Packing (implementazione A2, tipi riservati in A0): inline in foglia
  ≤ S1; pack sigillati ≤ S2 con mini-indice e **copy-out-on-write**
  (mai rewrite parziale); compattazione con la GC.

## 3. Superblock e layout (T3)

- LBA0 (+ shadow LBA1): `magic="ACFS"`, `version=1`, `block_size=3584`,
  `volume_uuid:u64`, `generation:u64`, `root_tree`, `refcount_root`,
  `alloc_hint`, `mountpoint[64]`, `auto:u8`, `flags` (dirty + feature),
  checksum FNV-1a self-verifying. Niente firma `55AA` (mai falsi mount FAT).
- Rilevabile con 1 read LBA0 + magic + bound (come `Fat32::mount`).
- Commit: shadow + flip; crash = generazione vecchia + orphan-GC
  (journal rivalutato solo se gli snapshot multipli lo impongono).
- Footer blocco 16B: `(type, device_idx, gen, checksum)` — blocchi
  self-describing, scrub indipendente dal tree.
- Pool uniforme con **hint di placement** (zone veloci, co-location:
  preferenze soft, mai vincoli); l'allocatore decide, A5 cambia politica.
- `device_table[8]` inline + `overflow_ptr` + `device_count`;
  `topology_gen` per i cambi (stale detection).
- Riservati: `net_cookie[16]` (A8), `vec_hook[8]` per oggetto (vector).

## 4. API nativa (T4)

- Dati: `R_OBJ_PUT/GET/DELETE/LIST` (+ chunk/commit espliciti oltre 4000B,
  indice chunk non offset); `GET_ID/STAT_ID` per UUID; `LIST` paginata con
  cursore opaco.
- Riservati: `R_SNAP_*` (A2), `R_ARCA_*` admin (A7).
- Errori `Result` tipizzati (`NOTFOUND/EXISTS/BUSY/NOSPACE`), mai errno.
- Pattern a 5 tocchi per ogni op: tag, expect, `op_bit`, wrapper libr, builtin.

## 5. Vista POSIX / mapping sintetico (T5)

- Oggetto = file, lista = readdir, dir **emergenti** (esistono ⟺ chiavi
  col prefisso; mai su disco).
- `mkdir` = transient set server-side per mount (cap 1024 negoziato,
  oltre `ERR_NOSPACE`) + asserzione bucket; assorbimento alla prima chiave;
  `rmdir` su transient-only = successo; tutto sparisce a unmount/reboot.
- Lettura + append + delete; write con offset = `ERR_INVALID`;
  `O_APPEND` unico parziale ammesso (naturale per COW).
- `stat`: size/kind/mtime dalla versione; `nlink` = versioni trattenute;
  `owner` = `creator_app` risolto (o hash corto); `group` = `-` (v1);
  `mode` = **proiezione ABAC valutata** (non memorizzata);
  `chmod` = `ERR_READONLY` (si usa il tool di policy).
- `atime` non tracciato in v1.

## 6. Diritti e ABAC (T6/T8)

- Bit `OBJ_R/OBJ_W` separati + `SNAP` + `ADMIN`; fail-closed agli ignoti;
  subtree esteso a bucket/prefisso (gratis: l'op porta bucket nel payload).
- Motore in userfs (mai kernel): soggetti = `app_hash` oggi (+ `app_sign`
  e UID domani come attributi), subtree, ruoli-servizio; oggetti = xattr.
- Ereditarieta' bucket→chiave solo in restrizione; snapshot con ACL
  congelata (o `ADMIN`); enforcement **ogni op** (chiude TOCTOU).
- FD = capability; anti-confused-deputy: i servizi valutano il chiamante
  originario (canale propagato).
- Per-applicazione nativo: bucket privati, entitlement dichiarati
  (bucket+verbi), sandbox per subtree, re-attest agli update (mai
  silent-widen oltre i verbi concessi).
- Estensioni future senza rework: bearer token per condivisione esterna,
  macaroon/delega attenuata con zecca-server, ABAC temporale per recenza
  versioni. Gli UID di Strato 3 saranno un attributo in piu'.

## 7. Quota e subvolumi (T7)

- Subvolume = mount con budget (`quota_blocks`, `used` senza doppio
  conteggio dei blocchi condivisi; contatori nell'object tree, scrub a
  verifica).
- Enforcement al put (`ERR_NOSPACE` prima di allocare, mai transazioni
  mezze scritte); snapshot contro il budget del subvolume che li trattiene.

## 8. Metadati (xattr + creator)

- Per versione (immutabili): `{creator_app, creator_uid (=0), creator_sign
  (=0), tick, size}` — `0` = non misurato all'epoca, mai wildcard.
- Per oggetto (mutabili, bump `ctime` senza nuova versione): xattr
  `user.*` liberi + `sys.*` riservati; chiavi ≤ 64B, valori ≤ 1KB,
  totale ≤ 2KB inline (oltre → blob attributi, pattern overflow).
- `ctime` = ultima modifica metadati/ACL; gli xattr alimentano ABAC
  (filtri) e vector (filtri RAG).

## 9. Tool `arca` (T9)

- `create/list/get/put/rm/snap/quota/policy/scrub/stat/swap` — un binario
  dedicato (non builtin: gira col proprio canale e i propri bit).
- Auto-mount: mountpoint + `auto` sul volume, scan boot (`DISK_LIST`),
  cache solo hint, conflitti dichiarati (doppio mountpoint = secondo
  inattivo + log).
- Il tool non scavalca: valuta il canale originario; `grant` mai oltre il
  tetto del concedente.

## 10. Multi-device, rete, swap, vector (T10+)

- RAID (A6): mirror prima (stesso extent, due `device_idx`), stripe dopo;
  commit client-side; device-id stabili da A0.
- Rete (A8): su device-id + generazioni; `net_cookie` + replica_set futuro.
- Swap: extent tipo `SWAP` + oggetto dimensionabile dal demone; sensori
  `SYS_MEMINFO`/`statvfs`/RSS; solo anonimo in v1 (text = scarta-ricarica,
  page-cache e shm = futuri); pager track separato dopo A2.
- Vector: servizio userspace separato (track parallelo V1, mai nel FS);
  embedding come oggetti derivati (`derived_from`, `model:`); RAG =
  similarita + filtri xattr → UUID → `GET_ID`.

## 11. Accesso database (supporto nel FS, motore fuori)

Il motore database (vettoriale o altro, ispirato a Jigen ma scritto per
Velordor in Rust come servizio userspace) vive **fuori** dal FS. Qui solo
le primitive di supporto — niente logica di indici, query o embedding:

- Modello I/O: **esplicito + cache nel servizio** (niente porting
  mmap-based). Il servizio pinna le strutture hot in heap e pesca il
  resto con `GET_ID`; niente fault-path, niente pager, niente deadlock.
- `R_OBJ_MGET`: batch generazionale (una IPC, N UUID → N blob) per fan-out
  tipo HNSW; generico, non vettor-specifico (serve anche a ls -l massivi,
  backup, scrub). Implementazione con A2, quando il profiling lo chiede.
- `R_SYNC` con modi None/Group/PerWrite: flush esplicito a gruppi
  (checkpoint ogni N + `SaveChanges`-like); con COW il commit sposta solo
  il puntatore, quindi Group costa quasi zero.
- Batch atomico multi-oggetto (BEGIN/COMMIT oltre il singolo re-key):
  per transazioni tipo shrink-swap e commit multi-file; torn-tail =
  scarto via checksum come il WAL di riferimento.
- `fallocate` + hint `sequential`/`random`: preallocazione run append
  (contiguita', mai ENOSPC a meta') e dichiarazione pattern per
  allocatore A5 e cache (ingestion sequenziale, HNSW random).
- Crash-marker documentato come pattern: lock-file + generation +
  dirty-bit (reconcile all'apertura, mai fsck cieco).
- Subvolumi per area (content/vectors/index = 3 subvolumi con quota
  propria) + snapshot O(1) al posto delle copie di backup.
- mmap file-backed: **fuori spec**, rivalutato solo su profiling
  (richiederebbe fault-path kernel anti-deadlock dedicato).
- Perche' piu' veloce di un FS normale: niente doppia cache (disco→ring→
  app in un viaggio), append COW senza journal metadati sul path caldo,
  placement che ascolta gli hint, zero-copy estendibile ai client FS.

## 12. Fasi (A1–A8 + V1)

- A1: singolo-device (format via `arca create`, negotiate, mount, R/W).
- A2: COW + snapshot/clone + GC (+ packing, + `R_OBJ_MGET`).
- A3: quota + subvolumi.
- A4: ACL/ABAC engine + tool policy.
- A5: device-awareness (`DISK_INFO`, TRIM, policy allocator, hint).
- A6: RAID. — A7: tool completo. — A8: rete.
- V1 (parallelo, mai nel FS): servizio vettoriale sopra §11.

## 13. Punti aperti (stima, non vincoli)

S1/S2 e extent minimo esatti; checksum footer (FNV vs CRC dedicato);
orphan-scan vs journal con snapshot multipli; `DISK_LIST`; formato
entitlement; threshold transient set; wall-clock oltre i tick; framing
multi-frame per `R_OBJ_MGET` oltre 4000B.
