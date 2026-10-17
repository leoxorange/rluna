//! A minimal PE32+ (Windows x64 executable) writer.
//!
//! The image has three sections:
//!   .text   code of all functions
//!   .data   writable data objects
//!   .idata  import directory, lookup tables and the import address table
//!
//! All code addresses symbols RIP-relatively, so the image needs no base
//! relocations; it is marked as non-relocatable and loads at `IMAGE_BASE`.

use std::collections::{BTreeMap, HashMap};

use super::Image;
use crate::codegen::{CodeFunc, RelocKind, import_symbol};
use crate::lir::Module;

/// The address Windows loads the image at (the conventional default for
/// 64-bit executables). Without base relocations the image *must* load
/// there, so the PE header marks it as not relocatable.
pub const IMAGE_BASE: u64 = 0x1_4000_0000;
/// Sections start on page boundaries in memory (4 KiB), because memory
/// permissions (read/write/execute) are set per page.
const SECTION_ALIGN: u32 = 0x1000;
/// In the file, sections only need 512-byte alignment, which keeps the
/// file small.
const FILE_ALIGN: u32 = 0x200;

fn align(v: u32, a: u32) -> u32 {
    v.div_ceil(a) * a
}

struct Section {
    name: &'static [u8; 8],
    data: Vec<u8>,
    rva: u32,
    characteristics: u32,
}

pub fn link(module: &Module, code: &[CodeFunc], entry: &str) -> Result<Image, String> {
    // .text
    let mut text = Vec::new();
    let mut func_offsets = HashMap::new();
    for f in code {
        while text.len() % 16 != 0 {
            text.push(0xCC);
        }
        func_offsets.insert(f.name.clone(), text.len() as u32);
        text.extend_from_slice(&f.code);
    }

    // .data
    let mut data = Vec::new();
    let mut data_offsets = HashMap::new();
    for d in &module.data {
        while data.len() % 8 != 0 {
            data.push(0);
        }
        data_offsets.insert(d.name.clone(), data.len() as u32);
        data.extend_from_slice(&d.bytes());
    }
    if data.is_empty() {
        data.push(0);
    }

    let text_rva = SECTION_ALIGN;
    let data_rva = text_rva + align(text.len() as u32, SECTION_ALIGN);
    let idata_rva = data_rva + align(data.len() as u32, SECTION_ALIGN);

    // .idata
    let mut dlls: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for e in &module.externs {
        dlls.entry(format!("{}.dll", e.library()))
            .or_default()
            .push((e.import_name().to_string(), e.name.clone()));
    }
    let (idata, iat_slots, import_dir, iat_dir) = build_imports(&dlls, idata_rva);

    // Resolve symbols.
    let mut symbols: HashMap<String, u32> = HashMap::new();
    for (name, off) in &func_offsets {
        symbols.insert(name.clone(), text_rva + off);
    }
    for (name, off) in &data_offsets {
        symbols.insert(name.clone(), data_rva + off);
    }
    for (name, rva) in &iat_slots {
        symbols.insert(import_symbol(name), *rva);
    }
    for f in code {
        let base = func_offsets[&f.name];
        for r in &f.relocs {
            assert_eq!(r.kind, RelocKind::Rel32, "PE images hold x86-64 code");
            let target = *symbols
                .get(&r.symbol)
                .ok_or_else(|| format!("undefined symbol `{}`", r.symbol))?;
            let at = base as usize + r.offset;
            let field_rva = text_rva as i64 + at as i64;
            let rel = target as i64 - (field_rva + 4);
            text[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
    }
    let entry_rva = *symbols
        .get(entry)
        .ok_or_else(|| format!("entry point `{entry}` is not defined"))?;

    let sections = vec![
        Section {
            name: b".text\0\0\0",
            data: text,
            rva: text_rva,
            characteristics: 0x6000_0020,
        },
        Section {
            name: b".data\0\0\0",
            data,
            rva: data_rva,
            characteristics: 0xC000_0040,
        },
        Section {
            name: b".idata\0\0",
            data: idata,
            rva: idata_rva,
            characteristics: 0xC000_0040,
        },
    ];

    let bytes = write_image(&sections, entry_rva, import_dir, iat_dir);
    let func_addrs = func_offsets
        .into_iter()
        .map(|(k, v)| (k, IMAGE_BASE + (v + text_rva) as u64))
        .collect();
    Ok(Image { bytes, func_addrs })
}

/// (section bytes, IAT slot RVA per extern symbol, import directory (rva, size), IAT (rva, size))
type Imports = (Vec<u8>, HashMap<String, u32>, (u32, u32), (u32, u32));

/// Builds the .idata section placed at RVA `base`.
///
/// For each DLL there is a directory entry pointing at two parallel tables:
/// the lookup table (which function names to import) and the import
/// address table (IAT). The loader reads the names, finds the functions in
/// the DLL, and overwrites each IAT entry with the function's address. Our
/// code calls through the IAT entries.
fn build_imports(dlls: &BTreeMap<String, Vec<(String, String)>>, base: u32) -> Imports {
    let ndlls = dlls.len() as u32;
    let total_funcs: u32 = dlls.values().map(|v| v.len() as u32).sum();
    let dir_size = (ndlls + 1) * 20;
    let table_size = (total_funcs + ndlls) * 8; // per dll: n entries + null
    let ilt_start = dir_size;
    let iat_start = ilt_start + table_size;
    let names_start = iat_start + table_size;

    let mut names = Vec::new();
    let mut out = vec![0u8; names_start as usize];
    let mut slots = HashMap::new();

    let mut table_off = 0u32;
    for (i, (dll, funcs)) in dlls.iter().enumerate() {
        let ilt = ilt_start + table_off;
        let iat = iat_start + table_off;
        for (j, (import, symbol)) in funcs.iter().enumerate() {
            // Hint/name entry: u16 hint, name, NUL, pad to even.
            let hn = names_start + names.len() as u32;
            names.extend_from_slice(&0u16.to_le_bytes());
            names.extend_from_slice(import.as_bytes());
            names.push(0);
            if names.len() % 2 != 0 {
                names.push(0);
            }
            let entry = (base + hn) as u64;
            let at = (ilt + 8 * j as u32) as usize;
            out[at..at + 8].copy_from_slice(&entry.to_le_bytes());
            let at = (iat + 8 * j as u32) as usize;
            out[at..at + 8].copy_from_slice(&entry.to_le_bytes());
            slots.insert(symbol.clone(), base + iat + 8 * j as u32);
        }
        let dll_name = names_start + names.len() as u32;
        names.extend_from_slice(dll.as_bytes());
        names.push(0);
        if names.len() % 2 != 0 {
            names.push(0);
        }
        let d = 20 * i;
        out[d..d + 4].copy_from_slice(&(base + ilt).to_le_bytes()); // OriginalFirstThunk
        out[d + 12..d + 16].copy_from_slice(&(base + dll_name).to_le_bytes()); // Name
        out[d + 16..d + 20].copy_from_slice(&(base + iat).to_le_bytes()); // FirstThunk
        table_off += (funcs.len() as u32 + 1) * 8;
    }
    out.extend_from_slice(&names);
    (out, slots, (base, dir_size), (base + iat_start, table_size))
}

fn write_image(
    sections: &[Section],
    entry_rva: u32,
    import_dir: (u32, u32),
    iat_dir: (u32, u32),
) -> Vec<u8> {
    let mut out = Vec::new();
    let u16le = |o: &mut Vec<u8>, v: u16| o.extend_from_slice(&v.to_le_bytes());
    let u32le = |o: &mut Vec<u8>, v: u32| o.extend_from_slice(&v.to_le_bytes());
    let u64le = |o: &mut Vec<u8>, v: u64| o.extend_from_slice(&v.to_le_bytes());

    // DOS header: only e_magic and e_lfanew matter. Every .exe still starts
    // with "MZ" for compatibility with MS-DOS; e_lfanew points to the real
    // PE header.
    let pe_offset = 0x40u32;
    out.extend_from_slice(b"MZ");
    out.resize(0x3C, 0);
    u32le(&mut out, pe_offset);

    // PE signature + COFF file header.
    out.extend_from_slice(b"PE\0\0");
    u16le(&mut out, 0x8664); // Machine: x86-64
    u16le(&mut out, sections.len() as u16);
    u32le(&mut out, 0); // TimeDateStamp (0 keeps builds reproducible)
    u32le(&mut out, 0); // PointerToSymbolTable
    u32le(&mut out, 0); // NumberOfSymbols
    u16le(&mut out, 240); // SizeOfOptionalHeader
    u16le(&mut out, 0x0001 | 0x0002 | 0x0020); // RELOCS_STRIPPED | EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE

    let headers_size = align(
        pe_offset + 4 + 20 + 240 + 40 * sections.len() as u32,
        FILE_ALIGN,
    );
    let last = sections.last().unwrap();
    let image_size = align(last.rva + last.data.len() as u32, SECTION_ALIGN);
    let raw_size = |s: &Section| align(s.data.len() as u32, FILE_ALIGN);
    let code_size: u32 = raw_size(&sections[0]);
    let init_data_size: u32 = sections[1..].iter().map(raw_size).sum();

    // Optional header (PE32+).
    u16le(&mut out, 0x20B);
    out.push(0); // MajorLinkerVersion
    out.push(1); // MinorLinkerVersion
    u32le(&mut out, code_size);
    u32le(&mut out, init_data_size);
    u32le(&mut out, 0); // SizeOfUninitializedData
    u32le(&mut out, entry_rva);
    u32le(&mut out, sections[0].rva); // BaseOfCode
    u64le(&mut out, IMAGE_BASE);
    u32le(&mut out, SECTION_ALIGN);
    u32le(&mut out, FILE_ALIGN);
    u16le(&mut out, 6); // MajorOperatingSystemVersion
    u16le(&mut out, 0);
    u16le(&mut out, 0); // MajorImageVersion
    u16le(&mut out, 0);
    u16le(&mut out, 6); // MajorSubsystemVersion
    u16le(&mut out, 0);
    u32le(&mut out, 0); // Win32VersionValue
    u32le(&mut out, image_size);
    u32le(&mut out, headers_size);
    u32le(&mut out, 0); // CheckSum
    u16le(&mut out, 3); // Subsystem: Windows console
    u16le(&mut out, 0x0100 | 0x8000); // NX_COMPAT | TERMINAL_SERVER_AWARE
    u64le(&mut out, 8 * 1024 * 1024); // SizeOfStackReserve
    u64le(&mut out, 64 * 1024); // SizeOfStackCommit
    u64le(&mut out, 1024 * 1024); // SizeOfHeapReserve
    u64le(&mut out, 4096); // SizeOfHeapCommit
    u32le(&mut out, 0); // LoaderFlags
    u32le(&mut out, 16); // NumberOfRvaAndSizes
    for i in 0..16 {
        let (rva, size) = match i {
            1 => import_dir,
            12 => iat_dir,
            _ => (0, 0),
        };
        u32le(&mut out, rva);
        u32le(&mut out, size);
    }

    // Section table.
    let mut file_off = headers_size;
    for s in sections {
        out.extend_from_slice(s.name);
        u32le(&mut out, s.data.len() as u32); // VirtualSize
        u32le(&mut out, s.rva);
        u32le(&mut out, raw_size(s));
        u32le(&mut out, file_off);
        u32le(&mut out, 0); // PointerToRelocations
        u32le(&mut out, 0); // PointerToLinenumbers
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u32le(&mut out, s.characteristics);
        file_off += raw_size(s);
    }

    out.resize(headers_size as usize, 0);
    for s in sections {
        out.extend_from_slice(&s.data);
        out.resize(align(out.len() as u32, FILE_ALIGN) as usize, 0);
    }
    out
}
