//! A minimal Mach-O writer for arm64 macOS executables.
//!
//! The image has four segments:
//!   __PAGEZERO  4 GiB of inaccessible memory below the image (traps null pointers)
//!   __TEXT      Mach-O header and load commands, then __text with all code
//!   __DATA      __data with the data objects, __got with the addresses of
//!               imported functions
//!   __LINKEDIT  bind opcodes, symbol tables, string table, code signature
//!
//! The image is position-independent: code addresses everything
//! PC-relatively (`adrp` + `add`/`ldr`) and the data holds no pointers, so it
//! needs no rebasing. dyld fills the GOT by following the bind opcodes.
//!
//! Apple silicon only runs signed code, so the image ends with an ad-hoc
//! code signature (a SHA-256 hash of every 4 KiB page of the file), the same
//! kind `ld` writes. No certificate is involved.
//!
//! A Mach-O file is a header followed by *load commands*: a list of
//! instructions for the kernel and dyld (the dynamic loader). "Map these
//! file bytes at this address with these permissions" (LC_SEGMENT_64),
//! "load this library" (LC_LOAD_DYLIB), "start here" (LC_MAIN), "here is
//! the signature" (LC_CODE_SIGNATURE), and so on. You can inspect them with
//! `otool -l <program>`.
//!
//! Why separate segments? Permissions. Code is read+execute but never
//! writable, and data is read+write but never executable. An attacker who
//! can write memory then cannot plant new code, and buggy code cannot
//! overwrite itself.

use std::collections::HashMap;

use super::Image;
use super::sha256::sha256;
use crate::codegen::{CodeFunc, RelocKind, import_symbol};
use crate::lir::Module;

/// Address of the start of __TEXT, right after __PAGEZERO.
pub const TEXT_BASE: u64 = 0x1_0000_0000;
/// Page size of arm64 macOS.
/// Segments must start on page boundaries, because memory permissions are
/// set per page. Apple silicon uses 16 KiB pages (most other systems use 4 KiB).
const PAGE: u64 = 0x4000;
/// Page size the code signature hashes.
const SIGN_PAGE: usize = 0x1000;

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xB;
const LC_LOAD_DYLIB: u32 = 0xC;
const LC_LOAD_DYLINKER: u32 = 0xE;
const LC_UUID: u32 = 0x1B;
const LC_CODE_SIGNATURE: u32 = 0x1D;
const LC_BUILD_VERSION: u32 = 0x32;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_MAIN: u32 = 0x8000_0028;

const DYLD_PATH: &str = "/usr/lib/dyld";
/// Index of __DATA among the segments, for bind opcodes.
const DATA_SEGMENT: u8 = 2;

fn align(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

/// Install name of a library named in an extern (`libSystem.write`).
fn install_name(library: &str) -> String {
    match library {
        "libSystem" => "/usr/lib/libSystem.B.dylib".into(),
        lib => format!("/usr/lib/{lib}.dylib"),
    }
}

/// Size of a load command holding a NUL-terminated string after `fixed` bytes.
fn str_cmd_size(fixed: u32, s: &str) -> u32 {
    align((fixed as usize + s.len() + 1) as u64, 8) as u32
}

struct Import {
    /// 1-based index into the LC_LOAD_DYLIB commands.
    ordinal: u8,
    /// C symbol name, e.g. `_write`.
    symbol: String,
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn be32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn be64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn name16(&mut self, name: &str) {
        let mut field = [0u8; 16];
        field[..name.len()].copy_from_slice(name.as_bytes());
        self.0.extend_from_slice(&field);
    }
    /// ULEB128: an unsigned number in 7-bit groups, low group first, with
    /// the top bit of each byte meaning "more bytes follow". Small numbers
    /// take one byte, which is why compact formats like bind opcodes and
    /// DWARF use it.
    fn uleb(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                self.u8(byte);
                return;
            }
            self.u8(byte | 0x80);
        }
    }
    /// A NUL-terminated string padded with zeros to `size` bytes in total
    /// counting from `start`.
    fn str_padded(&mut self, s: &str, start: usize, size: u32) {
        self.0.extend_from_slice(s.as_bytes());
        self.0.resize(start + size as usize, 0);
    }
    fn pad_to(&mut self, a: u64) {
        let len = align(self.0.len() as u64, a) as usize;
        self.0.resize(len, 0);
    }
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
}

struct Segment<'a> {
    name: &'a str,
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    prot: u32,
    sections: Vec<Section<'a>>,
}

struct Section<'a> {
    name: &'a str,
    addr: u64,
    size: u64,
    offset: u32,
    align_log2: u32,
    flags: u32,
    reserved1: u32,
}

fn segment_cmd(w: &mut Writer, s: &Segment) {
    w.u32(LC_SEGMENT_64);
    w.u32(72 + 80 * s.sections.len() as u32);
    w.name16(s.name);
    w.u64(s.vmaddr);
    w.u64(s.vmsize);
    w.u64(s.fileoff);
    w.u64(s.filesize);
    w.u32(s.prot); // maxprot
    w.u32(s.prot); // initprot
    w.u32(s.sections.len() as u32);
    w.u32(0);
    for sec in &s.sections {
        w.name16(sec.name);
        w.name16(s.name);
        w.u64(sec.addr);
        w.u64(sec.size);
        w.u32(sec.offset);
        w.u32(sec.align_log2);
        w.u32(0); // reloff
        w.u32(0); // nreloc
        w.u32(sec.flags);
        w.u32(sec.reserved1);
        w.u32(0); // reserved2
        w.u32(0); // reserved3
    }
}

/// Link the code of an AArch64 module into an executable. `identifier`
/// names the program in its code signature.
pub fn link(
    module: &Module,
    code: &[CodeFunc],
    entry: &str,
    identifier: &str,
) -> Result<Image, String> {
    // Libraries and imported symbols, in order of first use.
    let mut libraries: Vec<String> = Vec::new();
    let mut imports: Vec<Import> = Vec::new();
    let mut got_index: HashMap<String, usize> = HashMap::new();
    for e in &module.externs {
        let lib = install_name(e.library());
        let ordinal = match libraries.iter().position(|l| *l == lib) {
            Some(i) => i + 1,
            None => {
                libraries.push(lib);
                libraries.len()
            }
        };
        if ordinal > 15 {
            return Err("too many libraries".into());
        }
        got_index.insert(e.name.clone(), imports.len());
        imports.push(Import {
            ordinal: ordinal as u8,
            symbol: format!("_{}", e.import_name()),
        });
    }

    // Load commands. Their sizes are fixed, so the layout can be computed
    // before any of them is written.
    let dylib_sizes: Vec<u32> = libraries.iter().map(|l| str_cmd_size(24, l)).collect();
    let dylinker_size = str_cmd_size(12, DYLD_PATH);
    let ncmds = 4 + 8 + libraries.len() as u32;
    let sizeofcmds = 72 + (72 + 80) + (72 + 2 * 80) + 72 // segments
        + 48 + 24 + 80 + dylinker_size + 24 + 24 + 24 + 16
        + dylib_sizes.iter().sum::<u32>();
    let header_size = 32 + sizeofcmds as u64;

    // __TEXT: headers, then code.
    let text_off = align(header_size, 16);
    let mut text = Vec::new();
    let mut func_offsets = HashMap::new();
    for f in code {
        while text.len() % 16 != 0 {
            text.push(0);
        }
        func_offsets.insert(f.name.clone(), text.len() as u64);
        text.extend_from_slice(&f.code);
    }
    let text_size = align(text_off + text.len() as u64, PAGE);

    // __DATA: data objects, then the GOT.
    let data_base = TEXT_BASE + text_size;
    let mut data = Vec::new();
    let mut data_offsets = HashMap::new();
    for d in &module.data {
        while data.len() % 8 != 0 {
            data.push(0);
        }
        data_offsets.insert(d.name.clone(), data.len() as u64);
        data.extend_from_slice(&d.bytes());
    }
    while data.len() % 8 != 0 || data.is_empty() {
        data.push(0);
    }
    let got_off = data.len() as u64;
    data.resize(data.len() + 8 * imports.len(), 0);
    let data_size = align(data.len() as u64, PAGE);

    // Resolve symbols and patch the code.
    let mut symbols: HashMap<String, u64> = HashMap::new();
    for (name, off) in &func_offsets {
        symbols.insert(name.clone(), TEXT_BASE + text_off + off);
    }
    for (name, off) in &data_offsets {
        symbols.insert(name.clone(), data_base + off);
    }
    for (name, &i) in &got_index {
        symbols.insert(import_symbol(name), data_base + got_off + 8 * i as u64);
    }
    for f in code {
        let base = func_offsets[&f.name];
        for r in &f.relocs {
            let target = *symbols
                .get(&r.symbol)
                .ok_or_else(|| format!("undefined symbol `{}`", r.symbol))?;
            let at = (base as usize) + r.offset;
            let pc = TEXT_BASE + text_off + at as u64;
            let mut word = u32::from_le_bytes(text[at..at + 4].try_into().unwrap());
            word |= match r.kind {
                RelocKind::Branch26 => {
                    let delta = (target as i64 - pc as i64) >> 2;
                    if !(-(1 << 25)..1 << 25).contains(&delta) {
                        return Err(format!("branch to `{}` out of range", r.symbol));
                    }
                    (delta as u32) & 0x3FF_FFFF
                }
                // `adrp` wants the distance in 4 KiB pages between the
                // page of the instruction and the page of the target; the
                // low 12 bits come from the `add`/`ldr` that follows.
                RelocKind::Page21 => {
                    let pages = ((target & !0xFFF) as i64 - (pc & !0xFFF) as i64) >> 12;
                    let imm = pages as u32;
                    ((imm & 3) << 29) | (((imm >> 2) & 0x7FFFF) << 5)
                }
                RelocKind::AddLo12 => ((target & 0xFFF) as u32) << 10,
                RelocKind::Ldr64Lo12 => {
                    assert_eq!(target % 8, 0, "misaligned 64-bit load of `{}`", r.symbol);
                    (((target & 0xFFF) >> 3) as u32) << 10
                }
                RelocKind::Rel32 => unreachable!("Mach-O images hold AArch64 code"),
            };
            text[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    let entry_addr = *symbols
        .get(entry)
        .ok_or_else(|| format!("entry point `{entry}` is not defined"))?;

    // __LINKEDIT: bind opcodes, symbol table, indirect symbol table, strings.
    //
    // Bind opcodes are a tiny bytecode that dyld runs at startup: "use
    // library 1, symbol _write, write its address at __DATA + offset".
    // A bytecode instead of a table, because repeated settings (same
    // library, consecutive slots) cost nothing in a large program.
    let mut bind = Writer::default();
    for (i, imp) in imports.iter().enumerate() {
        bind.u8(0x10 | imp.ordinal); // BIND_OPCODE_SET_DYLIB_ORDINAL_IMM
        bind.u8(0x40); // BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM
        bind.0.extend_from_slice(imp.symbol.as_bytes());
        bind.u8(0);
        bind.u8(0x51); // BIND_OPCODE_SET_TYPE_IMM | BIND_TYPE_POINTER
        bind.u8(0x70 | DATA_SEGMENT); // BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB
        bind.uleb(got_off + 8 * i as u64);
        bind.u8(0x90); // BIND_OPCODE_DO_BIND
    }
    bind.u8(0); // BIND_OPCODE_DONE
    bind.pad_to(8);

    // Local symbols for the functions (for debuggers and `nm`), then one
    // undefined symbol per import.
    let mut strings = Writer::default();
    strings.0.extend_from_slice(b" \0");
    let mut symtab = Writer::default();
    let mut funcs: Vec<(&String, &u64)> = func_offsets.iter().collect();
    funcs.sort_by_key(|(_, off)| **off);
    for (name, off) in &funcs {
        symtab.u32(strings.len() as u32);
        strings.0.extend_from_slice(name.as_bytes());
        strings.u8(0);
        symtab.u8(0x0E); // N_SECT
        symtab.u8(1); // section 1: __text
        symtab.u16(0);
        symtab.u64(TEXT_BASE + text_off + **off);
    }
    for imp in &imports {
        symtab.u32(strings.len() as u32);
        strings.0.extend_from_slice(imp.symbol.as_bytes());
        strings.u8(0);
        symtab.u8(0x01); // N_UNDF | N_EXT
        symtab.u8(0);
        symtab.u16((imp.ordinal as u16) << 8); // library ordinal
        symtab.u64(0);
    }
    let nlocal = funcs.len() as u32;
    let nsyms = nlocal + imports.len() as u32;
    let mut indirect = Writer::default();
    for i in 0..imports.len() as u32 {
        indirect.u32(nlocal + i);
    }
    strings.pad_to(8);

    let linkedit_off = text_size + data_size;
    let bind_off = linkedit_off;
    let symtab_off = bind_off + bind.len();
    let indirect_off = symtab_off + symtab.len();
    let strings_off = indirect_off + indirect.len();
    let sig_off = align(strings_off + strings.len(), 16);
    let n_pages = (sig_off as usize).div_ceil(SIGN_PAGE);
    let cd_size = 88 + identifier.len() + 1 + 32 * n_pages;
    let sig_size = (20 + cd_size) as u64;
    let linkedit_size = sig_off + sig_size - linkedit_off;

    let uuid = {
        let mut content = text.clone();
        content.extend_from_slice(&data);
        let digest = sha256(&content);
        let mut u = [0u8; 16];
        u.copy_from_slice(&digest[..16]);
        u[6] = (u[6] & 0x0F) | 0x40; // version 4 layout
        u[8] = (u[8] & 0x3F) | 0x80;
        u
    };

    let segments = [
        Segment {
            name: "__PAGEZERO",
            vmaddr: 0,
            vmsize: TEXT_BASE,
            fileoff: 0,
            filesize: 0,
            prot: 0,
            sections: vec![],
        },
        Segment {
            name: "__TEXT",
            vmaddr: TEXT_BASE,
            vmsize: text_size,
            fileoff: 0,
            filesize: text_size,
            prot: 5, // r-x
            sections: vec![Section {
                name: "__text",
                addr: TEXT_BASE + text_off,
                size: text.len() as u64,
                offset: text_off as u32,
                align_log2: 4,
                flags: 0x8000_0400, // S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS
                reserved1: 0,
            }],
        },
        Segment {
            name: "__DATA",
            vmaddr: data_base,
            vmsize: data_size,
            fileoff: text_size,
            filesize: data_size,
            prot: 3, // rw-
            sections: vec![
                Section {
                    name: "__data",
                    addr: data_base,
                    size: got_off,
                    offset: text_size as u32,
                    align_log2: 3,
                    flags: 0,
                    reserved1: 0,
                },
                Section {
                    name: "__got",
                    addr: data_base + got_off,
                    size: 8 * imports.len() as u64,
                    offset: (text_size + got_off) as u32,
                    align_log2: 3,
                    flags: 0x6,   // S_NON_LAZY_SYMBOL_POINTERS
                    reserved1: 0, // first entry in the indirect symbol table
                },
            ],
        },
        Segment {
            name: "__LINKEDIT",
            vmaddr: data_base + data_size,
            vmsize: align(linkedit_size, PAGE),
            fileoff: linkedit_off,
            filesize: linkedit_size,
            prot: 1, // r--
            sections: vec![],
        },
    ];

    // Header and load commands.
    let mut w = Writer::default();
    w.u32(0xFEED_FACF); // MH_MAGIC_64
    w.u32(0x0100_000C); // CPU_TYPE_ARM64
    w.u32(0); // CPU_SUBTYPE_ARM64_ALL
    w.u32(2); // MH_EXECUTE
    w.u32(ncmds);
    w.u32(sizeofcmds);
    w.u32(0x1 | 0x4 | 0x80 | 0x20_0000); // NOUNDEFS | DYLDLINK | TWOLEVEL | PIE
    w.u32(0);
    for s in &segments {
        segment_cmd(&mut w, s);
    }

    w.u32(LC_DYLD_INFO_ONLY);
    w.u32(48);
    w.u32(0); // rebase
    w.u32(0);
    w.u32(bind_off as u32);
    w.u32(bind.len() as u32);
    for _ in 0..6 {
        w.u32(0); // weak bind, lazy bind, export
    }

    w.u32(LC_SYMTAB);
    w.u32(24);
    w.u32(symtab_off as u32);
    w.u32(nsyms);
    w.u32(strings_off as u32);
    w.u32(strings.len() as u32);

    w.u32(LC_DYSYMTAB);
    w.u32(80);
    w.u32(0); // ilocalsym
    w.u32(nlocal);
    w.u32(nlocal); // iextdefsym
    w.u32(0);
    w.u32(nlocal); // iundefsym
    w.u32(imports.len() as u32);
    for _ in 0..6 {
        w.u32(0); // toc, module table, external references
    }
    w.u32(indirect_off as u32);
    w.u32(imports.len() as u32);
    for _ in 0..4 {
        w.u32(0); // external and local relocations
    }

    let start = w.0.len();
    w.u32(LC_LOAD_DYLINKER);
    w.u32(dylinker_size);
    w.u32(12);
    w.str_padded(DYLD_PATH, start, dylinker_size);

    w.u32(LC_UUID);
    w.u32(24);
    w.0.extend_from_slice(&uuid);

    w.u32(LC_BUILD_VERSION);
    w.u32(24);
    w.u32(1); // PLATFORM_MACOS
    w.u32(11 << 16); // minos 11.0
    w.u32(11 << 16); // sdk 11.0
    w.u32(0); // ntools

    w.u32(LC_MAIN);
    w.u32(24);
    w.u64(entry_addr - TEXT_BASE); // file offset of the entry point
    w.u64(0); // default stack size

    for (lib, &size) in libraries.iter().zip(&dylib_sizes) {
        let start = w.0.len();
        w.u32(LC_LOAD_DYLIB);
        w.u32(size);
        w.u32(24); // name offset
        w.u32(2); // timestamp
        w.u32(0x0001_0000); // current version 1.0.0
        w.u32(0x0001_0000); // compatibility version 1.0.0
        w.str_padded(lib, start, size);
    }

    w.u32(LC_CODE_SIGNATURE);
    w.u32(16);
    w.u32(sig_off as u32);
    w.u32(sig_size as u32);
    assert_eq!(w.len(), header_size, "load command sizes");

    // Contents.
    w.0.resize(text_off as usize, 0);
    w.0.extend_from_slice(&text);
    w.0.resize(text_size as usize, 0);
    w.0.extend_from_slice(&data);
    w.0.resize(linkedit_off as usize, 0);
    w.0.extend_from_slice(&bind.0);
    w.0.extend_from_slice(&symtab.0);
    w.0.extend_from_slice(&indirect.0);
    w.0.extend_from_slice(&strings.0);
    w.0.resize(sig_off as usize, 0);

    // Code signature: a SuperBlob holding one CodeDirectory (big-endian).
    //
    // The kernel checks each page against its hash when the page is first
    // loaded into memory, and kills the process on a mismatch. So the hashes
    // must cover the final bytes of everything before the signature,
    // including the headers that describe the signature itself. That is why
    // the signature's size was computed earlier, before any header was
    // written, and why it is appended last.
    let hashes: Vec<[u8; 32]> = w.0.chunks(SIGN_PAGE).map(sha256).collect();
    let ident_off = 88u32;
    let hash_off = ident_off + identifier.len() as u32 + 1;
    w.be32(0xFADE_0CC0); // CSMAGIC_EMBEDDED_SIGNATURE
    w.be32(sig_size as u32);
    w.be32(1); // one blob
    w.be32(0); // CSSLOT_CODEDIRECTORY
    w.be32(20); // its offset
    w.be32(0xFADE_0C02); // CSMAGIC_CODEDIRECTORY
    w.be32(cd_size as u32);
    w.be32(0x20400); // version with exec segment fields
    w.be32(0x2_0002); // CS_ADHOC | CS_LINKER_SIGNED
    w.be32(hash_off);
    w.be32(ident_off);
    w.be32(0); // special slots
    w.be32(n_pages as u32);
    w.be32(sig_off as u32); // code limit
    w.u8(32); // hash size
    w.u8(2); // CS_HASHTYPE_SHA256
    w.u8(0); // platform
    w.u8(SIGN_PAGE.trailing_zeros() as u8);
    w.be32(0); // spare2
    w.be32(0); // scatter offset
    w.be32(0); // team offset
    w.be32(0); // spare3
    w.be64(0); // 64-bit code limit (unused)
    w.be64(0); // exec segment base
    w.be64(text_size); // exec segment limit
    w.be64(1); // CS_EXECSEG_MAIN_BINARY
    w.0.extend_from_slice(identifier.as_bytes());
    w.u8(0);
    for h in &hashes {
        w.0.extend_from_slice(h);
    }
    assert_eq!(w.len(), sig_off + sig_size, "code signature size");

    let func_addrs = func_offsets
        .into_iter()
        .map(|(k, v)| (k, TEXT_BASE + text_off + v))
        .collect();
    Ok(Image {
        bytes: w.0,
        func_addrs,
    })
}
