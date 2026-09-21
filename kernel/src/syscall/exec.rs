// Fase 37 (exec in-place): sys_exec.
use super::spawn::SPAWN_IMAGE_MAX;

/// exec_image(img_ptr, img_len): sostituisce l'immagine del CHIAMANTE con
/// l'ELF in sua memoria (Fase 37). Il kernel non tocca mai il FS (ADR-0005):
/// caricare da path e' compito di `libr::exec` (load_file + questa syscall).
/// Stesso bound di `spawn_image` (256 KiB); validazione ELF prima di toccare
/// qualunque stato (fallita = -1, processo intatto). Successo = nessun ritorno
/// (salto all'entry nuova); il valore 0 non e' mai osservato dal chiamante.
pub(super) fn sys_exec(img_ptr: u64, img_len: usize) -> i64 {
    if img_len == 0 || img_len > SPAWN_IMAGE_MAX {
        return -1;
    }
    if !crate::vmm_user::is_user_range(img_ptr, img_len) {
        crate::serial_println!("[syscall] exec: fuori dallo spazio user");
        return -1;
    }
    // Copia in heap kernel PRIMA del teardown (la sorgente user sparisce con
    // lo spazio che stiamo per smantellare — mai leggere user dopo).
    let src = unsafe { core::slice::from_raw_parts(img_ptr as *const u8, img_len) };
    let owned = src.to_vec();
    match crate::sched::exec_current(&owned) {
        Ok(()) => 0,
        Err(()) => -1,
    }
}
