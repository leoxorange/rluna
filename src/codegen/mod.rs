//! Machine code generation from LIR.
//!
//! Each backend turns every LIR function into a `CodeFunc`: its machine code,
//! the symbol references the linker must patch, and a listing for
//! `--emit=asm`.
//!
//! Instruction *selection* (which machine instructions implement each LIR
//! op) and instruction *encoding* (the exact bytes of each instruction)
//! are kept apart. Each backend has a `mod.rs` that walks the LIR and
//! decides what to emit, and an `asm.rs` that knows only how to turn one
//! instruction into bytes. The encoder can then be unit-tested against
//! a real assembler, and the selector reads like assembly language.

pub mod aarch64;
pub mod x86_64;

use std::fmt::Write;

/// How the linker patches a symbol reference at `Reloc::offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// x86-64: 32-bit field receives `target - (address of field + 4)`.
    Rel32,
    /// AArch64 `b`/`bl`: imm26 receives `(target - pc) / 4`.
    Branch26,
    /// AArch64 `adrp`: imm21 receives `page(target) - page(pc)` in pages.
    Page21,
    /// AArch64 `add`: imm12 receives the low 12 bits of the target.
    AddLo12,
    /// AArch64 64-bit `ldr`: imm12 receives the low 12 bits of the target / 8.
    Ldr64Lo12,
}

/// A reference to a symbol, resolved by the linker.
///
/// Why can't the code generator write the address itself? It compiles one
/// function at a time and does not know where the other functions and data
/// will end up in the executable. Only the linker decides the final layout.
/// So the code generator writes a placeholder (zeros) and records "patch
/// this spot with the address of `rt.print_int`": a *relocation*. The
/// linker fills in all of them once every address is known.
#[derive(Debug, Clone)]
pub struct Reloc {
    pub offset: usize,
    pub symbol: String,
    pub kind: RelocKind,
}

/// Machine code for one function.
pub struct CodeFunc {
    pub name: String,
    pub code: Vec<u8>,
    pub relocs: Vec<Reloc>,
    /// (code offset, text): instructions, labels (`.L…:`) and comments (`; …`).
    pub listing: Vec<(usize, String)>,
}

impl CodeFunc {
    /// Render the listing with addresses relative to `base`.
    pub fn render_listing(&self, base: u64) -> String {
        let is_marker = |t: &str| t.starts_with(".L") || t.starts_with(';');
        let mut out = String::new();
        let mut entries = self.listing.iter().peekable();
        while let Some((off, text)) = entries.next() {
            if is_marker(text) {
                writeln!(out, "{text}").unwrap();
                continue;
            }
            let end = entries
                .clone()
                .find(|(_, t)| !is_marker(t))
                .map(|(o, _)| *o)
                .unwrap_or(self.code.len());
            let bytes: Vec<String> = self.code[*off..end]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            writeln!(
                out,
                "  {:08x}:  {:<30} {text}",
                base + *off as u64,
                bytes.join(" ")
            )
            .unwrap();
        }
        out
    }
}

/// Symbol of the slot holding the address of an imported function: an import
/// address table entry (PE) or a GOT entry (Mach-O), filled in by the loader.
///
/// Functions from the OS (`write`, `WriteFile`) live in a system library
/// whose address is only known when the program starts, and changes from
/// run to run (address space layout randomization). So the code does not
/// call them directly. It loads their address from a slot in our own data,
/// and the OS loader writes the real address into that slot at startup.
pub fn import_symbol(extern_name: &str) -> String {
    format!("__imp.{extern_name}")
}
