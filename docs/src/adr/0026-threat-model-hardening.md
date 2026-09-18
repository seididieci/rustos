# ADR-0026: Threat model + hardening (kill/register/map, identità a strati)

## Status

Accepted (implementata, Fase 35 — gate 5/5 + 7/7 + 50/50 + shell 30/30).
Strato 2 (identità misurata) rimandato alla fase successiva.

## Context

Fino alla Fase 34 tutti i processi user si fidano a vicenda (modello
cooperativo da ricerca). Con `exec` + shell che lancia programmi di terzi
(Fase 36) il modello non regge più: serve lo sguardo avversariale. Verifica
sul codice (Fase 35): `sys_map_physical` non vincola il `phys` (qualunque
processo mappa qualunque RAM in RW: sandbox escape totale); `kill(pid)` non
controlla il chiamante (chiunque uccide qualunque server tranne init);
`service_register` non autentica (a slot libero, chi prima arriva); i server
vedono solo channel-id (nomi `SpawnMeta` autodichiarati, diritti Fase 17
auto-restrizione volontaria); FS senza access control (Strato 0, Fase 16b).

## Decision

**Avversario primario: programma locale malevolo** (binario utente ostile).
Secondario (fase successiva): immagine disco contraffatta. Fuori scopo:
side-channel, DMA fisico (solo PIO oggi, niente IOMMU), bug logici dei server,
attacchi fisici. **Radici di fiducia: bootloader + kernel + init + manifest.**

Strato 1 — cancelli kernel senza identità (Fase 35, prima di `exec`):

1. **Kill solo parent o init.** Uccidere un server supervisionato è operazione
   da supervisore: i test di restart guidano il caos *tramite init*
   (protocollo bounce: init uccide+riavvia i propri figli su richiesta). La
   shell documenta `kill` come parent-scoped (torna utile coi background job).
2. **Register servizi-sistema solo figli-di-init** (tutti i driver veri lo
   sono; `Test` resta aperto per la suite; `Init` libero ma gatato = sonda di
   test deterministica). Niente squat a slot libero dopo kill.
3. **`map_physical`/`map_in` solo frame propri del sistema**: record ring
   (`RING_PHYS`, qualunque processo: copre userfs→client), scratch dei test,
   frame VGA. Chiude il sandbox escape senza rompere alcun legit path.
4. **Policy `FS_REGISTER` in userfs** (con nuova syscall `peer_pid(chan)` +
   `ps_info` esistente): prefix deve stare sotto `/dev/` (niente hijack di
   `/`, niente voci rogue a root); replace di prefix esistente solo da
   init-child (niente squat persistente di `/dev/null` & co.); la morte purga
   comunque (Fase 14.11, self-healing).

Strato 2 — identità misurata (fase successiva, con o dopo `exec`):

5. Il kernel misura l'immagine allo spawn (hash nel PCB, come `text` già fa
   per lo sharing); `peer_info`/gate su hash noto per i servizi di sistema;
   policy mount in userfs su identità invece che su nomi. Il disco diventa
   non fidato per costruzione. La parentela resta come primo strato (difesa
   in profondità, non alternativa).

## Consequences

### Positive

- Chiude le tre catene d'attacco economiche (RAM arbitraria, kill→squat,
  hijack `/`); il resto è nuisance visibile o self-healing.
- Zero cambi di protocollo (solo rifiuti `-1`/`ERR` in più); la suite resta
  verde con adattamenti locali (bounce via init, ORPHAN al posto del kill in
  t40, `/dev/tdie` in t25).
- Coerente con ADR-0025: cancelli = meccanismo neutro; identità = attributo
  misurabile, non concetto POSIX (niente uid nel kernel).

### Negative

- `peer_pid` è +1 syscall (superficie minima, solo pid già visibili via `ps`).
- TOCTOU peer-morto→pid-riusato sul check register: mitigato dalla purga su
  EXIT_NOTIFY (finestra strettissima, danno = mount che muore al primo uso).
- `kill` della shell perde utilità fino ai background job (documentato).

### Neutral

- Test negativi dedicati (t50): kill altrui, register abusivo, map kernel,
  register `/` → tutti rifiutati; legit paths coperti dalla suite esistente.

## Alternatives Considered

- **Kill ad albero di nascita con walk:** scartato — i PID si riusano (niente
  generazioni) e il walk su numeri stale attribuisce male; la reparentazione
  a init romperebbe comunque t40.
- **FS_REGISTER solo init-child:** scartato — romperebbe t25 (MNTDIE non è
  figlio di init) senza benefici oltre la policy `/dev/` + no-squat.
- **econamel/allow-list di phys statiche:** scartato — i ring stanno ovunque;
  serve il criterio per-proprietà (record ring), non per-intervallo.
- **uid unix nel kernel:** scartato — concetto di personalità (ADR-0025 §1);
  l'identità nativa è hash misurato + parentela, mai uid.

## References

- `kernel/src/syscall/mem.rs` (`sys_map_physical`/`sys_map_in`),
  `kernel/src/sched_rt/lifecycle.rs` (`kill`), `kernel/src/channels.rs`
  (`register`), `userland/fs/src/server.rs` (`FS_REGISTER`)
- ADR-0025 (nucleo neutro, personalità), ADR-0010 (lifecycle/cascade),
  ADR-0014 (diritti per-canale), Fase 16b (Strato 0/permessi rimandati)
- Fase 35 (hardening), Fase 36 (`exec`), fase "identità misurata" (futura)
