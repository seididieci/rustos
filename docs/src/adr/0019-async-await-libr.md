# ADR-0019: async/await in libr sopra l'IPC asincrona

**Status**: In corso (Passi 1-2 completati e verificati, 3-4 pianificati)
**Data**: 2026-09-17

## Contesto

La Fase 13 (ADR-0009) ha dato a `libr` le primitive async (`send_async`,
`recv_nonblock`, `wait_reply` su `req_id`), ma usarle richiede state machine
scritte a mano: `FsReg::{step, collect_if_mine}` in userdisk, la SM di boot di
tty (phase=5), i collettori `fs_collect` nei client. Ogni nuovo flusso async
duplica il pattern send→ricorda-req→raccogli→correla, con i suoi bug
caratteristici (wake atteso pre-send, collect dimenticato, pending stale dopo
`EXIT_NOTIFY`).

In piu', `wait_reply` ha un limite strutturale: fallisce (`UnexpectedMsg`) se
arriva qualunque messaggio fuori ordine. Con N richieste in volo verso peer
diversi (o con richieste server in arrivo mentre si attende una reply), il
chiamante non puo' correlare da solo: serve un punto centrale che legga OGNI
messaggio e lo instradi al proprietario per `req_id`.

## Decisione

Sintassi `async/await` (solo `core::future`, niente dipendenze) sopra le
syscall 33/34 invariate, in un nuovo modulo `libr::task`, con un **router
centrale**: l'executor e' l'unico a chiamare `recv` e instrada ogni messaggio
(risposte → task proprietario per `req_id`, richieste → handler server,
`EXIT_NOTIFY` → waiter marcati con errore). La reply implicita kernel-side
resta: gli handler rispondono con `reply()` come oggi. Kernel **invariato**.

Livelli:

1. **Future di base** — `RecvMsg` (poll → `recv_poll`), `WaitReply{req}`,
   `SendAndWait{chan,tag,w0,w1}` combinata ("send che sembra sincrona").
2. **`block_on(fut)`** — single-task per client semplici (stessi limiti di
   `wait_reply`, ma componibile).
3. **`run()` multi-task** — const-generic su N task (array su stack,
      qualunque N, zero heap): polla i non-finiti; se nessuno → `recv()`
      bloccante → instrada a tutti gli accettanti (una reply ha un solo
      proprietario; un EXIT_NOTIFY pertinente sveglia ogni waiter).
4. **Waker custom** (`RawWaker`: wake = marca task ready; single-thread,
   niente lock). Pinning contenuto (`new_unchecked` su stack/array fermi, mai
   heap per-op, regola P1.2/scratch).

Vincoli ereditati dalla Fase 13 (NON rilassati qui): no mix sync/async per
processo; routing FIFO per `req_id` (niente riordino); FS 1-in-volo
(`FS_PENDING` invariata: il formato frame non ha lunghezza payload).

## Piano in 4 passi (stato: documentazione)

Ogni passo finisce con gate verde e review; se ci si ferma, la suite resta
verde al passo precedente.

- [x] **Passo 1 — `libr::task`** (Future `WaitReply`/`RecvMsg`, tratto
      `Receivable`, Waker no-op, `block_on`, `run` const-generic). Solo
      `libr`, nessun chiamante migrato. Verifica: build userland/testland,
      gate invariato 40/40 (rete di sicurezza come la Fase 13).
- [x] **Passo 2 — test t41/t42** (suite 40/40 → 42/42). t41: `block_on` +
      echo async verso helper `MODE_SRV` (reply routing per req_id). t42:
      `run()` con 2 task concorrenti + path morte server (`ServerDied`).
      Aggiornare run-tests.sh/AGENTS/docs-testing come nelle fasi passate.
- [x] **Passo 3 — client reale**: `FsRead` (compone `WaitReply::on_chan`:
      invio `read_async` a costruzione, attesa via router, `fs_collect_msg`
      non-bloccante al poll; stessi guard/formato/chan-filter). Copertura
      estendendo t20 (stessa lettura via collect manuale e via wrapper,
      confronto byte; fd riaperto: la prima lettura avanza la posizione).
- [ ] **Passo 4 — server pilota `userdisk`, SOLO registrazione**:
      `FsReg::{step,collect_if_mine}` → `async fn register(prefixes)` via
      `block_on` a startup, riusabile sul reset da `EXIT_NOTIFY`. Stessi log,
      stesso comportamento; loop `DISK_*`/`DEV_*` intatto. Copertura: t32
      (kill/restart userdisk). Il rewrite completo del loop e' un passo
      successivo separato (blast radius: data-plane critico di tutto).

## Conseguenze

- Positive: niente piu' SM scritte a mano per i nuovi flussi; `UnexpectedMsg`
  risolto per costruzione nel multi-task; base per futuri `join`/`select`;
  kernel e ABI IPC intoccati.
- Negative: `libr` cresce di poche centinaia di righe linkate in OGNI binario;
  `RawWaker` e' codice delicato (review + test dedicati); i vincoli Fase 13
  restano e vanno documentati per non illudere i chiamanti.
- Neutrali: i server esistenti non cambiano (tranne il pilota, Passo 4).

## Alternative scartate

- **Thread per task**: non esistono nel kernel (servirebbero clone, address
  space condivisi, sync primitive — una fase intera a se'). Scartata: i task
  sono cooperativi single-thread, come i server.
- **Executor con task dinamici illimitati**: pool statica N=8 invece —
  bound strutturali ovunque nel progetto, niente heap per-op.
- **Rewrite completo del loop userdisk subito**: troppo blast radius
  (data-plane critico); pilota limitato alla registrazione, loop al passo
  successivo.
- **`select`/`join` in questo passo**: utili ma non necessari per t41/t42 e
  pilota; rimandati (si scrivono sopra le stesse Future).

## Sviluppi futuri (non qui)

- `join`/`select` sopra le Future del Passo 1; timeout via `get_ticks`.
- Rewrite loop server completi in stile async (tty, userdisk data-plane).
- Rilassamento vincoli Fase 13 (mix sync/async, riordino locale, N-in-volo
  FS): richiede formato frame con lunghezza e/o `reply_to` esplicita.
- Log-service userspace come consumatore async (vedi discussione seriale).

## References

- ADR-0009 (IPC asincrono — primitive e vincoli ereditati)
- ADR-0008 (IPC per nome — canali, reply implicita)
- `libs/libr/src/lib.rs` (`send_async`, `recv_poll`, `wait_reply`)
