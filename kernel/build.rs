use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let boot_src = manifest_dir.join("src/boot.asm");
    let boot_obj = out_dir.join("boot.o");

    println!("cargo:rerun-if-changed={}", boot_src.display());

    // Il binario user embeddato (user_binary.rs) viene rigenerato da
    // scripts/build-userland.sh: se cambia il kernel va ricompilato.
    let user_bin = manifest_dir.join("../userland/build/userdemo.bin");
    println!("cargo:rerun-if-changed={}", user_bin.display());

    let status = Command::new("nasm")
        .args(["-f", "elf64"])
        .arg(&boot_src)
        .arg("-o")
        .arg(&boot_obj)
        .status()
        .expect("failed to run nasm for boot.asm");

    if !status.success() {
        panic!("nasm failed to assemble boot.asm");
    }

    // boot.o
    println!("cargo:rustc-link-arg-bins={}", boot_obj.display());

    // Link del kernel: -no-pie e il linker script sono specifici del KERNEL e
    // vanno emessi QUI (via link-arg del package), NON nelle rustflags della
    // root .cargo/config.toml. Se fossero nella config di root, si
    // infiltrerebbero (rustflags additive via discovery) anche nel link dei
    // binari userspace costruiti nella stessa radice.
    println!("cargo:rustc-link-arg-bins=-no-pie");
    let ld = manifest_dir.join("linker.ld");
    println!("cargo:rustc-link-arg-bins=-T{}", ld.display());
}
