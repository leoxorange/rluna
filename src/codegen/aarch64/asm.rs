//! A small AArch64 (A64) instruction encoder.
//!
//! Only the instructions the code generator needs are supported. Every
//! emitted instruction is also recorded as text so `--emit=asm` can produce
//! a readable listing.
//!
//! An "assembler" turns `add x9, x9, x10` into bytes. Each A64 instruction
//! is one 32-bit word: a fixed opcode pattern with fields for registers
//! and immediates OR-ed into place. For `add x9, x9, x10` = 0x8B0A0129:
//!
//! ```text
//!   10001011 000 01010 000000 01001 01001
//!   opcode       Rm=10        Rn=9  Rd=9
//! ```
//!
//! The opcode constants below come from the Arm Architecture Reference
//! Manual, and the unit tests compare the output with the system assembler.
//! Words are stored little-endian, the byte order Apple silicon uses.

use crate::codegen::{Reloc, RelocKind};

/// A general-purpose register. Register 31 is `sp` or `xzr` depending on
/// the instruction, as in the architecture. (The register field is 5 bits,
/// so 32 numbers; the 32nd means "stack pointer" for address computations
/// and "zero register" (reads 0, writes are discarded) elsewhere.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X(pub u8);

pub const X0: X = X(0);
pub const X9: X = X(9);
pub const X10: X = X(10);
/// Intra-procedure-call scratch register; used for large offsets and calls
/// through the GOT.
pub const X16: X = X(16);
pub const FP: X = X(29);
pub const LR: X = X(30);
pub const SP: X = X(31);
pub const XZR: X = X(31);

/// A floating-point register used as a 64-bit double (`d0`..`d31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct D(pub u8);

impl X {
    fn name(self) -> String {
        match self.0 {
            29 => "x29".into(),
            30 => "x30".into(),
            n => format!("x{n}"),
        }
    }
    /// Name when register 31 means `sp`.
    fn name_sp(self) -> String {
        if self.0 == 31 {
            "sp".into()
        } else {
            self.name()
        }
    }
    /// Name when register 31 means `xzr`.
    fn name_zr(self) -> String {
        if self.0 == 31 {
            "xzr".into()
        } else {
            self.name()
        }
    }
    fn w_name(self) -> String {
        format!("w{}", self.0)
    }
    fn n(self) -> u32 {
        self.0 as u32
    }
}

/// Condition codes: which combination of the flags N (negative), Z (zero),
/// C (carry) and V (overflow), as set by `cmp`/`fcmp`, counts as "true".
/// Codes come in pairs that differ only in the lowest bit and mean
/// opposite things (eq/ne, lt/ge, ...), which `cset` relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Eq = 0,
    Ne = 1,
    Hs = 2,
    Lo = 3,
    Mi = 4,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
}

impl Cond {
    fn name(self) -> &'static str {
        match self {
            Cond::Eq => "eq",
            Cond::Ne => "ne",
            Cond::Hs => "hs",
            Cond::Lo => "lo",
            Cond::Mi => "mi",
            Cond::Hi => "hi",
            Cond::Ls => "ls",
            Cond::Ge => "ge",
            Cond::Lt => "lt",
            Cond::Gt => "gt",
            Cond::Le => "le",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(pub usize);

/// Integer data-processing instructions with three registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Alu {
    Add = 0x8B00_0000,
    Sub = 0xCB00_0000,
    And = 0x8A00_0000,
    Orr = 0xAA00_0000,
    Eor = 0xCA00_0000,
    Mul = 0x9B00_7C00,
    Sdiv = 0x9AC0_0C00,
    Udiv = 0x9AC0_0800,
    Lsl = 0x9AC0_2000,
    Lsr = 0x9AC0_2400,
    Asr = 0x9AC0_2800,
}

impl Alu {
    fn name(self) -> &'static str {
        match self {
            Alu::Add => "add",
            Alu::Sub => "sub",
            Alu::And => "and",
            Alu::Orr => "orr",
            Alu::Eor => "eor",
            Alu::Mul => "mul",
            Alu::Sdiv => "sdiv",
            Alu::Udiv => "udiv",
            Alu::Lsl => "lsl",
            Alu::Lsr => "lsr",
            Alu::Asr => "asr",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FAlu {
    Add = 0x1E60_2800,
    Sub = 0x1E60_3800,
    Mul = 0x1E60_0800,
    Div = 0x1E60_1800,
}

/// Loads and stores. Each has three encodings: scaled unsigned 12-bit
/// offset, unscaled signed 9-bit offset, and register offset.
///
/// Three forms exist because a 32-bit instruction has little room for an
/// offset. "Scaled" counts in units of the access size, so 12 bits reach
/// 4095 × 8 = 32760 bytes for 8-byte accesses, but only forward. The
/// unscaled form reaches -256..255. Anything else needs the offset in a
/// register first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mem {
    LdrX,
    StrX,
    LdrB,
    StrB,
}

impl Mem {
    /// (scaled imm12, unscaled simm9, register offset, access size, mnemonic)
    fn encoding(self) -> (u32, u32, u32, i32, &'static str) {
        match self {
            Mem::LdrX => (0xF940_0000, 0xF840_0000, 0xF860_6800, 8, "ldr"),
            Mem::StrX => (0xF900_0000, 0xF800_0000, 0xF820_6800, 8, "str"),
            Mem::LdrB => (0x3940_0000, 0x3840_0000, 0x3860_6800, 1, "ldrb"),
            Mem::StrB => (0x3900_0000, 0x3800_0000, 0x3820_6800, 1, "strb"),
        }
    }
}

#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    pub relocs: Vec<Reloc>,
    labels: Vec<Option<usize>>,
    /// (instruction offset, label, conditional): conditional branches have
    /// a 19-bit offset, unconditional ones 26 bits.
    fixups: Vec<(usize, Label, bool)>,
    /// (code offset, text) for the listing.
    pub listing: Vec<(usize, String)>,
}

fn mem_text(base: X, off: i32) -> String {
    if off == 0 {
        format!("[{}]", base.name_sp())
    } else {
        format!("[{}, #{off}]", base.name_sp())
    }
}

impl Asm {
    fn emit(&mut self, word: u32, text: String) {
        self.listing.push((self.code.len(), text));
        self.code.extend_from_slice(&word.to_le_bytes());
    }

    fn emit_reloc(&mut self, word: u32, symbol: &str, kind: RelocKind, text: String) {
        self.relocs.push(Reloc {
            offset: self.code.len(),
            symbol: symbol.to_string(),
            kind,
        });
        self.emit(word, text);
    }

    pub fn comment(&mut self, text: impl Into<String>) {
        self.listing
            .push((self.code.len(), format!("; {}", text.into())));
    }

    // -- labels -----------------------------------------------------------

    pub fn new_label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    pub fn bind(&mut self, l: Label) {
        self.labels[l.0] = Some(self.code.len());
        self.listing.push((self.code.len(), format!(".L{}:", l.0)));
    }

    /// Patch label references. Call once all labels are bound.
    ///
    /// Branch offsets count instructions (4 bytes each) relative to the
    /// branch itself, which is why the byte distance is divided by 4.
    /// Conditional branches have only 19 bits for it (±1 MiB), plain `b`
    /// has 26 bits (±128 MiB).
    pub fn resolve_labels(&mut self) {
        for &(at, l, conditional) in &self.fixups {
            let target = self.labels[l.0].expect("unbound label");
            let delta = (target as i64 - at as i64) / 4;
            let mut word = u32::from_le_bytes(self.code[at..at + 4].try_into().unwrap());
            if conditional {
                assert!(
                    (-(1 << 18)..1 << 18).contains(&delta),
                    "conditional branch out of range"
                );
                word |= ((delta as u32) & 0x7FFFF) << 5;
            } else {
                word |= (delta as u32) & 0x3FF_FFFF;
            }
            self.code[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        self.fixups.clear();
    }

    // -- moves --------------------------------------------------------------

    /// mov rd, imm (movz/movn followed by movk as needed)
    ///
    /// A 64-bit constant cannot fit in a 32-bit instruction, so it is built
    /// 16 bits at a time. `movz` sets one 16-bit chunk and zeroes the rest,
    /// `movk` replaces one chunk and keeps the rest. For mostly-0xFFFF
    /// constants (negative numbers) `movn` starts from all-ones instead.
    /// Picking whichever filler is more common skips the most chunks:
    /// 5 takes one instruction, and so does -1.
    pub fn mov_imm(&mut self, rd: X, imm: i64) {
        let chunks: Vec<u32> = (0..4)
            .map(|i| ((imm >> (16 * i)) & 0xFFFF) as u32)
            .collect();
        let ones = chunks.iter().filter(|&&c| c == 0xFFFF).count();
        let zeros = chunks.iter().filter(|&&c| c == 0).count();
        let inverted = ones > zeros;
        let filler = if inverted { 0xFFFF } else { 0 };
        // The first instruction sets every other chunk to the filler.
        let lead = chunks.iter().position(|&c| c != filler).unwrap_or(0);
        for (hw, &c) in chunks.iter().enumerate() {
            if c == filler && hw != lead {
                continue;
            }
            let hw = hw as u32;
            if hw as usize == lead {
                let (op, field, name) = if inverted {
                    (0x9280_0000, !c & 0xFFFF, "movn")
                } else {
                    (0xD280_0000, c, "movz")
                };
                self.emit(
                    op | hw << 21 | field << 5 | rd.n(),
                    format!("{name} {}, #{field:#x}, lsl #{}", rd.name(), 16 * hw),
                );
            } else {
                self.emit(
                    0xF280_0000 | hw << 21 | c << 5 | rd.n(),
                    format!("movk {}, #{c:#x}, lsl #{}", rd.name(), 16 * hw),
                );
            }
        }
    }

    /// mov rd, rm (neither may be sp)
    pub fn mov(&mut self, rd: X, rm: X) {
        self.emit(
            0xAA00_03E0 | rm.n() << 16 | rd.n(),
            format!("mov {}, {}", rd.name(), rm.name_zr()),
        );
    }

    /// add rd, rn, #imm / sub rd, rn, #imm (rd and rn may be sp)
    pub fn add_imm(&mut self, rd: X, rn: X, imm: i64) {
        let (op, name, mag) = if imm >= 0 {
            (0x9100_0000u32, "add", imm as u64)
        } else {
            (0xD100_0000u32, "sub", imm.unsigned_abs())
        };
        if mag < 1 << 12 {
            self.emit(
                op | (mag as u32) << 10 | rn.n() << 5 | rd.n(),
                format!("{name} {}, {}, #{mag}", rd.name_sp(), rn.name_sp()),
            );
        } else {
            // Extended-register form, which accepts sp.
            self.mov_imm(X16, mag as i64);
            let op = if imm >= 0 { 0x8B20_6000 } else { 0xCB20_6000 };
            self.emit(
                op | X16.n() << 16 | rn.n() << 5 | rd.n(),
                format!("{name} {}, {}, x16", rd.name_sp(), rn.name_sp()),
            );
        }
    }

    /// adrp rd, symbol@PAGE ; add rd, rd, symbol@PAGEOFF
    ///
    /// How to get an address without knowing where the program will be
    /// loaded: `adrp` computes "the 4 KiB page that is N pages away from
    /// this instruction", and `add` adds the low 12 bits within the page.
    /// Both depend only on the *distance* between code and data, which the
    /// linker knows, so the code works wherever the OS loads it.
    pub fn adr_symbol(&mut self, rd: X, symbol: &str) {
        self.emit_reloc(
            0x9000_0000 | rd.n(),
            symbol,
            RelocKind::Page21,
            format!("adrp {}, {symbol}@PAGE", rd.name()),
        );
        self.emit_reloc(
            0x9100_0000 | rd.n() << 5 | rd.n(),
            symbol,
            RelocKind::AddLo12,
            format!("add {}, {}, {symbol}@PAGEOFF", rd.name(), rd.name()),
        );
    }

    /// `adrp rd, slot@PAGE ; ldr rd, [rd, slot@PAGEOFF]`
    pub fn load_symbol(&mut self, rd: X, symbol: &str) {
        self.emit_reloc(
            0x9000_0000 | rd.n(),
            symbol,
            RelocKind::Page21,
            format!("adrp {}, {symbol}@PAGE", rd.name()),
        );
        self.emit_reloc(
            0xF940_0000 | rd.n() << 5 | rd.n(),
            symbol,
            RelocKind::Ldr64Lo12,
            format!("ldr {}, [{}, {symbol}@PAGEOFF]", rd.name(), rd.name()),
        );
    }

    // -- memory ---------------------------------------------------------------

    /// Load or store `rt` at `[base + off]`, choosing an encoding that fits
    /// the offset (x16 holds offsets that fit none).
    pub fn mem(&mut self, op: Mem, rt: X, base: X, off: i32) {
        let (scaled, unscaled, regoff, size, name) = op.encoding();
        let rt_name = if size == 1 { rt.w_name() } else { rt.name() };
        self.mem_raw(
            scaled,
            unscaled,
            regoff,
            size,
            name,
            rt.n(),
            rt_name,
            base,
            off,
        );
    }

    /// ldr dt, [base + off]
    pub fn ldr_d(&mut self, dt: D, base: X, off: i32) {
        let name = format!("d{}", dt.0);
        self.mem_raw(
            0xFD40_0000,
            0xFC40_0000,
            0xFC60_6800,
            8,
            "ldr",
            dt.0 as u32,
            name,
            base,
            off,
        );
    }

    /// str dt, [base + off]
    pub fn str_d(&mut self, dt: D, base: X, off: i32) {
        let name = format!("d{}", dt.0);
        self.mem_raw(
            0xFD00_0000,
            0xFC00_0000,
            0xFC20_6800,
            8,
            "str",
            dt.0 as u32,
            name,
            base,
            off,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn mem_raw(
        &mut self,
        scaled: u32,
        unscaled: u32,
        regoff: u32,
        size: i32,
        name: &str,
        rt: u32,
        rt_name: String,
        base: X,
        off: i32,
    ) {
        let regs = base.n() << 5 | rt;
        if off >= 0 && off % size == 0 && off / size < 1 << 12 {
            let imm = (off / size) as u32;
            self.emit(
                scaled | imm << 10 | regs,
                format!("{name} {rt_name}, {}", mem_text(base, off)),
            );
        } else if (-256..256).contains(&off) {
            let imm = (off as u32) & 0x1FF;
            let uname = format!("{}u{}", &name[..2], &name[2..]); // ldr -> ldur
            self.emit(
                unscaled | imm << 12 | regs,
                format!("{uname} {rt_name}, {}", mem_text(base, off)),
            );
        } else {
            self.mov_imm(X16, off as i64);
            self.emit(
                regoff | X16.n() << 16 | regs,
                format!("{name} {rt_name}, [{}, x16]", base.name_sp()),
            );
        }
    }

    /// `stp x29, x30, [sp, #-16]!`
    pub fn push_frame(&mut self) {
        self.emit(0xA9BF_7BFD, "stp x29, x30, [sp, #-16]!".into());
    }

    /// `ldp x29, x30, [sp], #16`
    pub fn pop_frame(&mut self) {
        self.emit(0xA8C1_7BFD, "ldp x29, x30, [sp], #16".into());
    }

    // -- integer arithmetic --------------------------------------------------

    /// op rd, rn, rm
    pub fn alu(&mut self, op: Alu, rd: X, rn: X, rm: X) {
        self.emit(
            op as u32 | rm.n() << 16 | rn.n() << 5 | rd.n(),
            format!("{} {}, {}, {}", op.name(), rd.name(), rn.name(), rm.name()),
        );
    }

    /// msub rd, rn, rm, ra (rd = ra - rn * rm)
    pub fn msub(&mut self, rd: X, rn: X, rm: X, ra: X) {
        self.emit(
            0x9B00_8000 | rm.n() << 16 | ra.n() << 10 | rn.n() << 5 | rd.n(),
            format!(
                "msub {}, {}, {}, {}",
                rd.name(),
                rn.name(),
                rm.name(),
                ra.name()
            ),
        );
    }

    /// cmp rn, rm
    pub fn cmp(&mut self, rn: X, rm: X) {
        self.emit(
            0xEB00_001F | rm.n() << 16 | rn.n() << 5,
            format!("cmp {}, {}", rn.name(), rm.name()),
        );
    }

    /// cset rd, cond (rd = cond ? 1 : 0)
    ///
    /// `cset` is not a real instruction but an alias: `csinc rd, xzr, xzr,
    /// !cond` ("rd = !cond ? 0 : 0 + 1"). Hence the inverted condition in
    /// the encoding.
    pub fn cset(&mut self, rd: X, cond: Cond) {
        let inverted = cond as u32 ^ 1;
        self.emit(
            0x9A9F_07E0 | inverted << 12 | rd.n(),
            format!("cset {}, {}", rd.name(), cond.name()),
        );
    }

    // -- floating point --------------------------------------------------------

    /// fadd/fsub/fmul/fdiv dd, dn, dm
    pub fn falu(&mut self, op: FAlu, dd: D, dn: D, dm: D) {
        let name = match op {
            FAlu::Add => "fadd",
            FAlu::Sub => "fsub",
            FAlu::Mul => "fmul",
            FAlu::Div => "fdiv",
        };
        self.emit(
            op as u32 | (dm.0 as u32) << 16 | (dn.0 as u32) << 5 | dd.0 as u32,
            format!("{name} d{}, d{}, d{}", dd.0, dn.0, dm.0),
        );
    }

    /// fcmp dn, dm
    pub fn fcmp(&mut self, dn: D, dm: D) {
        self.emit(
            0x1E60_2000 | (dm.0 as u32) << 16 | (dn.0 as u32) << 5,
            format!("fcmp d{}, d{}", dn.0, dm.0),
        );
    }

    /// scvtf dd, xn
    pub fn scvtf(&mut self, dd: D, xn: X) {
        self.emit(
            0x9E62_0000 | xn.n() << 5 | dd.0 as u32,
            format!("scvtf d{}, {}", dd.0, xn.name()),
        );
    }

    /// fcvtzs xd, dn
    pub fn fcvtzs(&mut self, xd: X, dn: D) {
        self.emit(
            0x9E78_0000 | (dn.0 as u32) << 5 | xd.n(),
            format!("fcvtzs {}, d{}", xd.name(), dn.0),
        );
    }

    // -- control flow ------------------------------------------------------------

    pub fn b(&mut self, l: Label) {
        self.fixups.push((self.code.len(), l, false));
        self.emit(0x1400_0000, format!("b .L{}", l.0));
    }

    /// cbz/cbnz rt, label
    pub fn cbz(&mut self, rt: X, nonzero: bool, l: Label) {
        self.fixups.push((self.code.len(), l, true));
        let (op, name) = if nonzero {
            (0xB500_0000, "cbnz")
        } else {
            (0xB400_0000, "cbz")
        };
        self.emit(op | rt.n(), format!("{name} {}, .L{}", rt.name(), l.0));
    }

    /// bl symbol
    pub fn bl(&mut self, symbol: &str) {
        self.emit_reloc(
            0x9400_0000,
            symbol,
            RelocKind::Branch26,
            format!("bl {symbol}"),
        );
    }

    /// blr rn
    pub fn blr(&mut self, rn: X) {
        self.emit(0xD63F_0000 | rn.n() << 5, format!("blr {}", rn.name()));
    }

    pub fn ret(&mut self) {
        self.emit(0xD65F_03C0, "ret".into());
    }

    /// brk #1 (trap)
    pub fn brk(&mut self) {
        self.emit(0xD420_0020, "brk #1".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(f: impl FnOnce(&mut Asm)) -> Vec<u32> {
        let mut a = Asm::default();
        f(&mut a);
        a.code
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    // Expected values come from the system assembler (`clang -c`).
    #[test]
    fn encodings() {
        // movz x9, #0x5 ; movn x9, #0x0 (mov x9, -1)
        assert_eq!(words(|a| a.mov_imm(X9, 5)), vec![0xD28000A9]);
        assert_eq!(words(|a| a.mov_imm(X9, -1)), vec![0x92800009]);
        assert_eq!(words(|a| a.mov_imm(X9, 0)), vec![0xD2800009]);
        // mov x9, #0x7fffffffffffffff
        assert_eq!(words(|a| a.mov_imm(X9, i64::MAX)), vec![0x92F00009]);
        // mov x9, #0x123456789 = movz #0x6789 ; movk #0x2345, lsl 16 ; movk #0x1, lsl 32
        assert_eq!(
            words(|a| a.mov_imm(X9, 0x1_2345_6789)),
            vec![0xD28CF129, 0xF2A468A9, 0xF2C00029]
        );
        // mov x0, x9
        assert_eq!(words(|a| a.mov(X0, X9)), vec![0xAA0903E0]);
        // mov x29, sp ; sub sp, sp, #32 ; mov sp, x29
        assert_eq!(words(|a| a.add_imm(FP, SP, 0)), vec![0x910003FD]);
        assert_eq!(words(|a| a.add_imm(SP, SP, -32)), vec![0xD10083FF]);
        assert_eq!(words(|a| a.add_imm(SP, FP, 0)), vec![0x910003BF]);
        // mov x16, #0x10000 ; sub sp, sp, x16
        assert_eq!(
            words(|a| a.add_imm(SP, SP, -0x10000)),
            vec![0xD2A00030, 0xCB3063FF]
        );
        // ldr x9, [sp, #16] ; str x9, [x29, #-8] (stur) ; ldr x9, [sp, x16]
        assert_eq!(words(|a| a.mem(Mem::LdrX, X9, SP, 16)), vec![0xF9400BE9]);
        assert_eq!(words(|a| a.mem(Mem::StrX, X9, FP, -8)), vec![0xF81F83A9]);
        assert_eq!(
            words(|a| a.mem(Mem::LdrX, X9, SP, 40000)),
            vec![0xD2938810, 0xF8706BE9]
        );
        // ldrb w9, [x9, #8] ; strb w10, [x9]
        assert_eq!(words(|a| a.mem(Mem::LdrB, X9, X9, 8)), vec![0x39402129]);
        assert_eq!(words(|a| a.mem(Mem::StrB, X10, X9, 0)), vec![0x3900012A]);
        // ldr d0, [sp, #24] ; str d1, [sp, #8]
        assert_eq!(words(|a| a.ldr_d(D(0), SP, 24)), vec![0xFD400FE0]);
        assert_eq!(words(|a| a.str_d(D(1), SP, 8)), vec![0xFD0007E1]);
        // stp x29, x30, [sp, #-16]! ; ldp x29, x30, [sp], #16
        assert_eq!(words(|a| a.push_frame()), vec![0xA9BF7BFD]);
        assert_eq!(words(|a| a.pop_frame()), vec![0xA8C17BFD]);
        // add / sub / mul / sdiv / udiv / lsl / asr x9, x9, x10
        assert_eq!(words(|a| a.alu(Alu::Add, X9, X9, X10)), vec![0x8B0A0129]);
        assert_eq!(words(|a| a.alu(Alu::Sub, X9, X9, X10)), vec![0xCB0A0129]);
        assert_eq!(words(|a| a.alu(Alu::Mul, X9, X9, X10)), vec![0x9B0A7D29]);
        assert_eq!(words(|a| a.alu(Alu::Sdiv, X9, X9, X10)), vec![0x9ACA0D29]);
        assert_eq!(words(|a| a.alu(Alu::Udiv, X9, X9, X10)), vec![0x9ACA0929]);
        assert_eq!(words(|a| a.alu(Alu::Lsl, X9, X9, X10)), vec![0x9ACA2129]);
        assert_eq!(words(|a| a.alu(Alu::Asr, X9, X9, X10)), vec![0x9ACA2929]);
        // msub x9, x16, x10, x9
        assert_eq!(words(|a| a.msub(X9, X16, X10, X9)), vec![0x9B0AA609]);
        // cmp x9, x10 ; cset x9, lt
        assert_eq!(words(|a| a.cmp(X9, X10)), vec![0xEB0A013F]);
        assert_eq!(words(|a| a.cset(X9, Cond::Lt)), vec![0x9A9FA7E9]);
        // fadd d0, d0, d1 ; fdiv d0, d0, d1 ; fcmp d0, d1
        assert_eq!(
            words(|a| a.falu(FAlu::Add, D(0), D(0), D(1))),
            vec![0x1E612800]
        );
        assert_eq!(
            words(|a| a.falu(FAlu::Div, D(0), D(0), D(1))),
            vec![0x1E611800]
        );
        assert_eq!(words(|a| a.fcmp(D(0), D(1))), vec![0x1E612000]);
        // scvtf d0, x9 ; fcvtzs x9, d0
        assert_eq!(words(|a| a.scvtf(D(0), X9)), vec![0x9E620120]);
        assert_eq!(words(|a| a.fcvtzs(X9, D(0))), vec![0x9E780009]);
        // blr x16 ; ret ; brk #1
        assert_eq!(words(|a| a.blr(X16)), vec![0xD63F0200]);
        assert_eq!(words(|a| a.ret()), vec![0xD65F03C0]);
        assert_eq!(words(|a| a.brk()), vec![0xD4200020]);
    }

    #[test]
    fn branches() {
        // .L0: cbnz x9, .L1 ; b .L0 ; .L1:
        let w = words(|a| {
            let l0 = a.new_label();
            let l1 = a.new_label();
            a.bind(l0);
            a.cbz(X9, true, l1);
            a.b(l0);
            a.bind(l1);
            a.resolve_labels();
        });
        assert_eq!(w, vec![0xB5000049, 0x17FFFFFF]);
    }
}
