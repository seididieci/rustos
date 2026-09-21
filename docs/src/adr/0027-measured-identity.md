# ADR-0027: Identità misurata (Strato 2 di ADR-0026)

## Status

Accepted (implementata, Fase 36 — gate 5/5 + 7/7 + 51/51 + shell 30/30).

## Context

ADR-0026 (Strato 1, Fase 35) ha chiuso i cancelli senza identità: kill
parent-scoped, register solo figli-di-init, `map_*` per-proprietà, policy
`FS_REGISTER` su parentela. Restava il punto 5: il kernel misura l'immagine
allo spawn (hash nel PCB, come `text` già fa per lo sharing) e la policy
vive su identità invece che su nomi. Il disco (`/fat`, da cui init carica i
servizi via `spawn_image`) è non fidato per costruzione: senza verifica, un
disco manomesso diventa codice con i pieni poteri del servizio sostituito.

## Decision

**Misura nel kernel, policy fuori** (kernel neutro, ADR-0025):

1. **Misura.** `syscall_numbers::image_hash` (FNV-1a64, single source
   kernel+user; lo sharing `text` adotta la stessa funzione, t47 invariato).
   Il PCB porta `image_hash: u64`, calcolato in `create_user` sui byte ELF
   validati (gli stessi che il loader mappa — misura ciò che gira, mai ciò
   che il chiamante dichiara), ereditato dal fork; 0 = processi kernel.
2. **Esposizione.** `SYS_PEER_INFO (47)`: hash del peer di un canale
   (0 = nascita), `rax = 0 + rdi = hash`, -1 a canale morto. Wrapper
   `libr::peer_info`. Non rivela nulla oltre l'identità del binario
   (nomi/pid già pubblici via `ps`).
3. **Manifest generato.** `scripts/gen-service-hashes.sh` calcola FNV-1a sui
   `.bin` finali (gli stessi byte embeddati via `include_bytes!` e copiati
   verbatim su `/fat` via mcopy) e scrive `build-meta/service_hashes.rs`
   (`HASH_*`), incluso via `VELORDOR_SERVICE_HASHES` (senza: compile fail-loud).
   `build-userland.sh` è riordinata (bin → gen → fs, init); `build-tests.sh`
   riesporta la variabile per t51. `build-meta/` è in `.gitignore`.
4. **Policy init (36.4).** `spawn_file` ricalcola l'hash dei byte caricati e
   lo confronta col manifest; mismatch = fail-loud a boot, retry-con-hold in
   supervisione. Log `hash-ok` solo a verifica avvenuta. Embedded disk/fs
   esclusi (TCB del kernel, init non ha i byte in mano); test esclusi
   (non-servizi).
5. **Policy `FS_REGISTER` (36.5).** Il replace di un prefix vivo riesce dallo
   STESSO binario (`peer_info` nuovo == `peer_info` driver vivo: il restart
   da disco rilegge gli stessi byte, funziona senza init), da init-child
   (bootstrap) o a driver morto (stale). Prima registrazione sotto `/dev/`
   resta aperta (t25). `driver_name_of` per audit nei log (mai decisioni).
6. **Test t51.** `peer_info` su Console/Devfs == manifest; stabilità tra
   istanze; same-image positivo (X2 rimpiazza X1 vivo, il mount sopravvive al
   kill — con le sole regole 35 sarebbe rifiutato); squat con hash diverso
   rifiutato (mount purgato); `peer_info` a canale morto → Err. Helper
   `REG51` (/dev/t51, stesso binario) + ramo SQUAT in `usertest-spin`
   (binario diverso).

## Consequences

### Positive

- Chiude la catena "disco manomesso → servizio sostituito" per i servizi da
  disco (6/6 verificati a ogni boot + restart) e lo squat persistente senza
  init (serve lo stesso binario o init).
- Zero cambi di protocollo (solo rifiuti in più); il gate parentela resta
  come primo strato (difesa in profondità).
- Restart da disco senza init: l'identità sostituisce il privilegio
  (stessi byte → stesso hash → replace consentito).

### Negative

- Il manifest incorpora hash: un binario che incorpora il manifest non può
  contenere il proprio hash (ciclo instabile, mai fixpoint — osservato:
  `HASH_USERFS` flippava a ogni run). Regola: il manifest esclude
  `userinit`/`userfs`; su userfs embedded non c'è pinning da manifest (la
  regola same-image non ne ha bisogno).
- `peer_info` è +1 syscall (stessa giustificazione di `peer_pid`: solo dati
  già visibili via `ps`).

### Neutral

- Diritti Fase 17 restano auto-restrizione volontaria; identità misurata e
  diritti si compongono senza toccarsi (l'una dice CHI sei, gli altri COSA
  puoi fare sul canale).
- Strato 3 futuro (identità per policy mount su nomi → UUID già stabili;
  credenziali/login boundary) resta fuori scope.

## Alternatives Considered

- **Allow-list hash nel kernel** (gate in `sys_service_register`): scartata —
  il kernel conoscerebbe identità = contro ADR-0025 (personalità fuori dal
  kernel); la parentela + policy userspace bastano.
- **Manifest su disco** (`/fat/manifest`): scartata — il disco è non fidato
  per costruzione; il manifest è TCB e vive nell'immagine init (come il
  kernel che la embedda).
- **Manifest checked-in a mano**: scartato — ogni rebuild cambierebbe i byte
  (o no, ma la verifica sarebbe manuale); generazione a build-time = sempre
  coerente, fail-loud altrimenti.
- **Pinning per-slot nel kernel** (solo hash X può registrare Fs): scartato —
  stessa obiezione della allow-list + rigidità al restart (cambia il binario,
  devi cambiare il kernel).

## References

- ADR-0026 (threat model, Strato 1), ADR-0025 (kernel neutro), ADR-0008
  (canali), ADR-0024 (fork: eredita l'hash), Fase 32 (`text`: stesso algoritmo)
- `syscall-numbers` (`image_hash`, `SYS_PEER_INFO`), `kernel/src/process.rs`
  (`image_hash`), `kernel/src/syscall/service.rs` (`sys_peer_info`),
  `scripts/gen-service-hashes.sh`, `userland/init/src/main.rs`
  (`spawn_file`), `userland/fs/src/server.rs` (`FS_REGISTER`, same-image)
- Fase 36 (questa fase), t51 (`testland/usertests/src/t_stable.rs`)
