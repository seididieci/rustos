//! Process Control Block: descrive un singolo processo utente/kernel.

use x86_64::structures::gdt::SegmentSelector;
use crate::context::CpuContext;

/// Stack kernel di un processo (inizializzato nel frattempo, fuori dall'heap
/// affinche' non venga mai spostato). Grandezza fissa in frame fisici.
pub const STACK_FRAMES: usize = 4; // 4 × 4 KiB = 16 KiB

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Pronto per essere schedulato.
    Ready,
    /// In attesa di un evento (es. scancode in coda).
    Blocked,
    /// Finito (non piu' schedulabile).
    Terminated,
}

/// Messaggio IPC (ADR-0008): contenuto registro-based trasportato da una
/// `send` su canale a un `recv`. Riposto nel PCB del ricevente (mai in PERCPU
/// perche' e' una zona transitoria single-slot).
#[derive(Clone, Copy, Debug)]
pub struct PendingMsg {
    /// Canale su cui il messaggio e' arrivato (identifica il client quando un
    /// server serve piu' canali). Sostituisce il `sender` per-PID.
    pub channel: usize,
    /// Request-id del messaggio (Fase 13, IPC async). Assegnato dal MITTENTE
    /// (`req_next`): `>= 0` = richiesta; `< 0` = risposta async a `-req_id`.
    pub req_id: i64,
    pub tag: u64,
    pub w0: u64,
    pub w1: u64,
}

/// Risposta IPC in viaggio verso un mittente che sta aspettando la `reply` del
/// suo server. E' il corpo della risposta; il destinatario e' implicito (sta
/// nel `reply_slot` del processo che la riceve).
#[derive(Clone, Copy, Debug)]
pub struct PendingReply {
    pub tag: u64,
    pub w0: u64,
    pub w1: u64,
}

/// Coda circulara a dimensione fissa per i messaggi IPC in entrata.
/// Embeddita nel PCB (nessuna heap allocation), O(1) push/pop.
const MSG_QUEUE_CAP: usize = 8;

pub struct MsgQueue {
    buf: [PendingMsg; MSG_QUEUE_CAP],
    head: usize,
    len: usize,
}

impl MsgQueue {
    pub const fn new() -> Self {
        Self {
            buf: [PendingMsg { channel: 0, req_id: 0, tag: 0, w0: 0, w1: 0 }; MSG_QUEUE_CAP],
            head: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, msg: PendingMsg) {
        if self.len >= MSG_QUEUE_CAP {
            return; // coda piena — non dovrebbe accadere in pratica
        }
        let tail = (self.head + self.len) % MSG_QUEUE_CAP;
        self.buf[tail] = msg;
        self.len += 1;
    }

    /// Come `push`, ma ritorna `false` se la coda e' piena invece di scartare
    /// in silenzio. Usato da `send_async` per dare backpressure al mittente
    /// (Fase 13): un frame non consegnato → errore, niente messaggi persi.
    pub fn try_push(&mut self, msg: PendingMsg) -> bool {
        if self.len >= MSG_QUEUE_CAP {
            return false;
        }
        let tail = (self.head + self.len) % MSG_QUEUE_CAP;
        self.buf[tail] = msg;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<PendingMsg> {
        if self.len == 0 {
            return None;
        }
        let msg = self.buf[self.head];
        self.head = (self.head + 1) % MSG_QUEUE_CAP;
        self.len -= 1;
        Some(msg)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Stato IPC di un processo: nessuna attesa, oppure bloccato in `recv` o in
/// attesa di una `reply`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpcState {
    None,
    /// Sto aspettando un messaggio (sono in `recv`).
    BlockedOnRecv,
    /// Ho fatto `send` e aspetto la `reply` del destinatario.
    BlockedOnReply,
}

/// Entry di un processo: ciclo infinito, mai ritornare.
pub type ProcessFn = unsafe extern "C" fn() -> !;

/// Massimo numero di peer distinti notificabili alla morte di un processo
/// (Fase 14, notifica unificata): i peer sono PID diversi da se' stesso, con
/// max 32 PID concorrenti → 31 e' un bound provabile (nessuna policy di
/// overflow necessaria). Piu' canali verso lo stesso peer collassano in una
/// sola entry (first-channel-wins): una notifica per peer basta.
pub const MAX_NOTIFY_PEERS: usize = 31;

pub struct Process {
    #[allow(dead_code)]
    pub id: usize,
    pub name: &'static str,
    /// Priorita' BASE del processo (0 = massima, 31 = minima). Immutabile.
    pub priority: crate::sched::Priority,
    pub state: State,
    /// Processo padre (chi ha creato questo processo via `spawn`). `None` per i
    /// processi creati direttamente dal kernel (es. init, idle, keyboard).
    #[allow(dead_code)]
    pub parent: Option<usize>,
    /// Indirizzo base (basso) dello stack kernel, per un futuro rilascio.
    #[allow(dead_code)]
    pub stack_base: u64,
    /// Indirizzo alto dello stack kernel.
    #[allow(dead_code)]
    pub stack_top: u64,
    /// CR3 (page table) del processo. Per i processi kernel e' la CR3 di base.
    pub cr3: u64,
    /// Top dello stack kernel usato come RSP0 del TSS quando il processo
    /// gira in user mode (per processi kernel coincide con `stack_top`).
    pub kernel_stack_top: u64,
    /// Contesto CPU salvato (puntato anche dall'assembly, mai spostato).
    pub saved: CpuContext,
    /// Selettore GDT del TSS per-processo (RSP0 + I/O bitmap). Caricato con
    /// `ltr` a ogni context switch (ADR-0006).
    pub tss_sel: SegmentSelector,
    /// Slot del TSS nel pool (1..MAX_TSS_SLOTS-1). Distinto dal `tss_sel`
    /// (indice GDT = base + slot): serve a `free_tss_slot` al reclaim
    /// (Fase 14).
    pub tss_slot: usize,
    /// Stato IPC corrente (Fase 7 / ADR-0008).
    pub ipc_state: IpcState,
    /// Messaggi in coda per questo processo (da `send` su canale in attesa di
    /// `recv`). Ogni messaggio porta il channel sorgente (ADR-0008).
    pub msg_queue: MsgQueue,
    /// Canale di nascita verso il parent (ADR-0008): il figlio lo riceve alla
    /// creazione e lo usa come "canale 0" per parlare col parent. `None` per i
    /// processi kernel (init/idle/keyboard) che non hanno un parent user.
    pub parent_chan: Option<usize>,
    /// Il canale del messaggio che questo processo sta correntemente
    /// elaborando (impostato da `recv`): la prossima `reply` risponde su quel
    /// canale. Con piu' client concorrenti, il server risponde al messaggio che
    /// ha appena ricevuto (fix 9.2.2, generalizzato a canali).
    pub reply_chan: Option<usize>,
    /// Il `req_id` del messaggio correntemente elaborato (salvato da `recv`
    /// insieme a `reply_chan`, Fase 13). La `reply` del server a un client
    /// async accoda una risposta con `req_id = -reply_req`.
    pub reply_req: i64,
    /// Contatore per il prossimo request-id: ogni `send`/`send_async` di questo
    /// processo assegna `req_id = req_next` poi incrementa (Fase 13).
    pub req_next: u64,
    /// La risposta che sto aspettando (riempita dal server alla `reply`).
    pub reply_slot: Option<PendingReply>,
    /// Evento di risveglio arrivato mentre il processo NON era ancora Bloccato
    /// (race producer/consumer dei wait da IRQ, es. kbd): settato da `wake`,
    /// consumato da `block_current`. Evita che il blocco perda il wake.
    pub pending_wake: bool,
    /// Indice del server CBS associato a questo processo (`None` = nessun
    /// server). Sempre presente: lo scheduler RT unico supporta il CBS.
    pub cbs_server: Option<usize>,
    /// Codice di uscita (Fase 14): significativo quando `state == Terminated`.
    pub exit_code: i64,
    /// Pid sul quale questo processo e' bloccato in attesa di una reply
    /// (`ipc_state == BlockedOnReply`, Fase 14). Usato per svegliare i mittenti
    /// sincroni quando il destinatario muore, evitando il deadlock client-su-
    /// servizio-morto.
    pub waiting_pid: Option<usize>,
    /// Coppie `(peer, channel)` da notificare con `EXIT_NOTIFY` al reclaim
    /// (notifica unificata, Fase 14): enumerate in `terminate` prima di
    /// `release_pid`, consumate in `reclaim_one` dopo il teardown. Solo i
    /// primi `die_peer_count` elementi sono validi.
    pub die_peers: [(u32, u32); MAX_NOTIFY_PEERS],
    /// Numero di entry valide in `die_peers`.
    pub die_peer_count: usize,
}

impl Process {
    /// Crea un processo **kernel**. `parent` = pid del creatore (albero
    /// processi, radicato in init), `parent_chan` = canale di nascita verso il
    /// creatore (`None` per init/idle/... creati dal kernel).
    pub fn create(
        id: usize,
        name: &'static str,
        priority: crate::sched::Priority,
        entry: ProcessFn,
        parent: Option<usize>,
        parent_chan: Option<usize>,
        io_ranges: &'static [(u16, u16)],
    ) -> Option<Process> {
        let stack_base = crate::phys_mem::alloc_contiguous(STACK_FRAMES)?;
        let stack_top = stack_base + (STACK_FRAMES as u64 * crate::phys_mem::FRAME_SIZE);

        // Stack kernel in 16 KiB: finestra per il frame CPU fittizio e i
        // frame di interrupt annidati.
        let saved = unsafe { crate::context::new_context(stack_top, entry as usize as u64) };

        let tss_slot = Self::alloc_tss(stack_top, io_ranges)?;
        let tss_sel = crate::gdt::selectors().tss_selector(tss_slot);

        Some(Process {
            id,
            name,
            priority,
            state: State::Ready,
            parent,
            stack_base,
            stack_top,
            cr3: crate::vmm_user::kernel_cr3(),
            kernel_stack_top: stack_top,
            saved,
            tss_sel,
            tss_slot,
            ipc_state: IpcState::None,
            msg_queue: MsgQueue::new(),
            parent_chan,
            reply_chan: None,
            reply_req: 0,
            req_next: 1,
            reply_slot: None,
            pending_wake: false,
            cbs_server: None,
            exit_code: 0,
            waiting_pid: None,
            die_peers: [(0, 0); MAX_NOTIFY_PEERS],
            die_peer_count: 0,
        })
    }

    /// Crea un processo **user** (gira in ring 3, Fase 6.2).
    ///
    /// Alloca il kernel stack (per `RSP0` e i frame di interrupt), crea un
    /// address space dedicato (`new_address_space`), vi mappa il codice
    /// `code_phys` (per `code_frames` frame) a `USER_CODE` e lo stack user, e
    /// prepara un frame CPU ring 3 (`new_context_user`) con `entry` come RIP.
    ///
    /// # Safety
    /// `code_phys` deve puntare a frame fisici validi con il codice user.
    /// `parent` e' il processo che richiede la creazione (`None` se dal kernel).
    /// `io_ranges` = porte I/O (inclusive) consentite a ring 3 (TSS ADR-0006).
    pub unsafe fn create_user(
        id: usize,
        name: &'static str,
        priority: crate::sched::Priority,
        code_phys: u64,
        code_frames: usize,
        entry: u64,
        parent: Option<usize>,
        parent_chan: Option<usize>,
        io_ranges: &'static [(u16, u16)],
    ) -> Option<Process> {
        // Kernel stack: RSP0 (per rientrare a ring 0 su interrupt) + frame.
        let stack_base = crate::phys_mem::alloc_contiguous(STACK_FRAMES)?;
        let stack_top = stack_base + (STACK_FRAMES as u64 * crate::phys_mem::FRAME_SIZE);

        // Address space user dedicato (PML4 proprio, kernel condiviso U=0).
        let cr3 = crate::vmm_user::new_address_space()?;

        // Mappa codice + stack user, e prende il RSP iniziale. La pagina FS
        // per-processo viene allocata/mappata lazy al primo uso (syscall 26).
        let user_stack_top =
            unsafe { crate::vmm_user::setup_user_memory(cr3, code_phys, code_frames) };

        // Frame CPU ring 3 sul kernel stack.
        let saved =
            unsafe { crate::context::new_context_user(stack_top, entry, user_stack_top) };

        let tss_slot = Self::alloc_tss(stack_top, io_ranges)?;
        let tss_sel = crate::gdt::selectors().tss_selector(tss_slot);

        Some(Process {
            id,
            name,
            priority,
            state: State::Ready,
            parent,
            stack_base,
            stack_top,
            cr3,
            kernel_stack_top: stack_top,
            saved,
            tss_sel,
            tss_slot,
            ipc_state: IpcState::None,
            msg_queue: MsgQueue::new(),
            parent_chan,
            reply_chan: None,
            reply_req: 0,
            req_next: 1,
            reply_slot: None,
            pending_wake: false,
            cbs_server: None,
            exit_code: 0,
            waiting_pid: None,
            die_peers: [(0, 0); MAX_NOTIFY_PEERS],
            die_peer_count: 0,
        })
    }

    /// Alloca uno slot TSS dal pool, lo configura (RSP0 + IST + bitmap I/O) e
    /// ritorna lo SLOT del pool (1-based). Il selettore GDT e' derivabile con
    /// `gdt::selectors().tss_selector(slot)`.
    fn alloc_tss(stack_top: u64, io_ranges: &'static [(u16, u16)]) -> Option<usize> {
        let slot = crate::gdt::alloc_tss_slot()?;
        crate::gdt::configure_tss(slot, x86_64::VirtAddr::new(stack_top), io_ranges);
        Some(slot)
    }
}
