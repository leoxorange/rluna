//! A small x86-64 instruction encoder.
//!
//! Only the instructions the code generator needs are supported. Every
//! emitted instruction is also recorded as text so `--emit=asm` can produce
//! a readable listing.

use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reg(pub u8);

pub const RAX: Reg = Reg(0);
pub const RCX: Reg = Reg(1);
pub const RDX: Reg = Reg(2);
pub const RSP: Reg = Reg(4);
pub const RBP: Reg = Reg(5);
pub const R8: Reg = Reg(8);
pub const R9: Reg = Reg(9);

/// Integer argument registers of the Windows x64 calling convention.
pub const ARG_REGS: [Reg; 4] = [RCX, RDX, R8, R9];

const REG_NAMES: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];
const REG8_NAMES: [&str; 16] = [
    "al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil", "r8b", "r9b", "r10b", "r11b", "r12b",
    "r13b", "r14b", "r15b",
];

impl Reg {
    fn name(self) -> &'static str {
        REG_NAMES[self.0 as usize]
    }
    fn name8(self) -> &'static str {
        REG8_NAMES[self.0 as usize]
    }
    fn low(self) -> u8 {
        self.0 & 7
    }
    fn high(self) -> u8 {
        (self.0 >> 3) & 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xmm(pub u8);

impl Xmm {
    fn high(self) -> u8 {
        (self.0 >> 3) & 1
    }
}

/// Condition codes (the low nibble of Jcc/SETcc opcodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    B = 0x2,
    AE = 0x3,
    E = 0x4,
    NE = 0x5,
    BE = 0x6,
    A = 0x7,
    P = 0xA,
    NP = 0xB,
    L = 0xC,
    GE = 0xD,
    LE = 0xE,
    G = 0xF,
}

impl Cond {
    fn name(self) -> &'static str {
        match self {
            Cond::B => "b",
            Cond::AE => "ae",
            Cond::E => "e",
            Cond::NE => "ne",
            Cond::BE => "be",
            Cond::A => "a",
            Cond::P => "p",
            Cond::NP => "np",
            Cond::L => "l",
            Cond::GE => "ge",
            Cond::LE => "le",
            Cond::G => "g",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(pub usize);

/// A 32-bit PC-relative reference to a symbol, resolved by the linker:
/// the field at `offset` receives `target - (address of field + 4)`.
#[derive(Debug, Clone)]
pub struct Reloc {
    pub offset: usize,
    pub symbol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alu {
    Add = 0x03,
    Or = 0x0B,
    And = 0x23,
    Sub = 0x2B,
    Xor = 0x33,
    Cmp = 0x3B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sse {
    Add = 0x58,
    Mul = 0x59,
    Sub = 0x5C,
    Div = 0x5E,
}

#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    pub relocs: Vec<Reloc>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label)>,
    /// (code offset, text) for the listing.
    pub listing: Vec<(usize, String)>,
}

fn mem(base: Reg, disp: i32) -> String {
    if disp == 0 {
        format!("[{}]", base.name())
    } else if disp < 0 {
        format!("[{} - {}]", base.name(), -(disp as i64))
    } else {
        format!("[{} + {}]", base.name(), disp)
    }
}

impl Asm {
    fn note(&mut self, start: usize, text: String) {
        self.listing.push((start, text));
    }

    pub fn comment(&mut self, text: impl Into<String>) {
        self.listing
            .push((self.code.len(), format!("; {}", text.into())));
    }

    fn byte(&mut self, b: u8) {
        self.code.push(b);
    }

    fn i32(&mut self, v: i32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    fn rex(&mut self, w: bool, r: u8, x: u8, b: u8, force: bool) {
        let rex = 0x40 | ((w as u8) << 3) | (r << 2) | (x << 1) | b;
        if rex != 0x40 || force {
            self.byte(rex);
        }
    }

    /// ModRM (+SIB) for `[base + disp32]` with the given reg field.
    fn modrm_mem(&mut self, reg: u8, base: Reg, disp: i32) {
        self.byte(0x80 | ((reg & 7) << 3) | base.low());
        if base.low() == 4 {
            self.byte(0x24); // SIB: base only
        }
        self.i32(disp);
    }

    fn modrm_rr(&mut self, reg: u8, rm: u8) {
        self.byte(0xC0 | ((reg & 7) << 3) | (rm & 7));
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
    pub fn resolve_labels(&mut self) {
        for &(at, l) in &self.fixups {
            let target = self.labels[l.0].expect("unbound label");
            let rel = target as i64 - (at as i64 + 4);
            self.code[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        self.fixups.clear();
    }

    // -- moves --------------------------------------------------------------

    /// mov dst, [base + disp]
    pub fn load(&mut self, dst: Reg, base: Reg, disp: i32) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, base.high(), false);
        self.byte(0x8B);
        self.modrm_mem(dst.0, base, disp);
        self.note(s, format!("mov {}, qword {}", dst.name(), mem(base, disp)));
    }

    /// mov [base + disp], src
    pub fn store(&mut self, base: Reg, disp: i32, src: Reg) {
        let s = self.code.len();
        self.rex(true, src.high(), 0, base.high(), false);
        self.byte(0x89);
        self.modrm_mem(src.0, base, disp);
        self.note(s, format!("mov qword {}, {}", mem(base, disp), src.name()));
    }

    /// movzx dst, byte [base + disp]
    pub fn load_u8(&mut self, dst: Reg, base: Reg, disp: i32) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, base.high(), false);
        self.byte(0x0F);
        self.byte(0xB6);
        self.modrm_mem(dst.0, base, disp);
        self.note(s, format!("movzx {}, byte {}", dst.name(), mem(base, disp)));
    }

    /// mov byte [base + disp], src8
    pub fn store_u8(&mut self, base: Reg, disp: i32, src: Reg) {
        let s = self.code.len();
        self.rex(false, src.high(), 0, base.high(), true);
        self.byte(0x88);
        self.modrm_mem(src.0, base, disp);
        self.note(s, format!("mov byte {}, {}", mem(base, disp), src.name8()));
    }

    /// mov dst, src
    pub fn mov_rr(&mut self, dst: Reg, src: Reg) {
        let s = self.code.len();
        self.rex(true, src.high(), 0, dst.high(), false);
        self.byte(0x89);
        self.modrm_rr(src.0, dst.0);
        self.note(s, format!("mov {}, {}", dst.name(), src.name()));
    }

    /// mov dst, imm
    pub fn mov_imm(&mut self, dst: Reg, imm: i64) {
        let s = self.code.len();
        if imm >= i32::MIN as i64 && imm <= i32::MAX as i64 {
            self.rex(true, 0, 0, dst.high(), false);
            self.byte(0xC7);
            self.modrm_rr(0, dst.0);
            self.i32(imm as i32);
        } else {
            self.rex(true, 0, 0, dst.high(), false);
            self.byte(0xB8 + dst.low());
            self.code.extend_from_slice(&imm.to_le_bytes());
        }
        self.note(s, format!("mov {}, {imm}", dst.name()));
    }

    /// lea dst, [base + disp]
    pub fn lea(&mut self, dst: Reg, base: Reg, disp: i32) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, base.high(), false);
        self.byte(0x8D);
        self.modrm_mem(dst.0, base, disp);
        self.note(s, format!("lea {}, {}", dst.name(), mem(base, disp)));
    }

    /// lea dst, [rip + symbol]
    pub fn lea_symbol(&mut self, dst: Reg, symbol: &str) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, 0, false);
        self.byte(0x8D);
        self.byte(((dst.0 & 7) << 3) | 0b101);
        self.relocs.push(Reloc {
            offset: self.code.len(),
            symbol: symbol.to_string(),
        });
        self.i32(0);
        self.note(s, format!("lea {}, [rip + {symbol}]", dst.name()));
    }

    // -- integer arithmetic --------------------------------------------------

    /// op dst, src
    pub fn alu(&mut self, op: Alu, dst: Reg, src: Reg) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, src.high(), false);
        self.byte(op as u8);
        self.modrm_rr(dst.0, src.0);
        let name = match op {
            Alu::Add => "add",
            Alu::Or => "or",
            Alu::And => "and",
            Alu::Sub => "sub",
            Alu::Xor => "xor",
            Alu::Cmp => "cmp",
        };
        self.note(s, format!("{name} {}, {}", dst.name(), src.name()));
    }

    /// imul dst, src
    pub fn imul(&mut self, dst: Reg, src: Reg) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, src.high(), false);
        self.byte(0x0F);
        self.byte(0xAF);
        self.modrm_rr(dst.0, src.0);
        self.note(s, format!("imul {}, {}", dst.name(), src.name()));
    }

    pub fn cqo(&mut self) {
        let s = self.code.len();
        self.byte(0x48);
        self.byte(0x99);
        self.note(s, "cqo".into());
    }

    /// xor edx, edx
    pub fn zero_rdx(&mut self) {
        let s = self.code.len();
        self.byte(0x31);
        self.byte(0xD2);
        self.note(s, "xor edx, edx".into());
    }

    /// idiv src (signed) or div src (unsigned): rdx:rax / src
    pub fn div(&mut self, src: Reg, signed: bool) {
        let s = self.code.len();
        self.rex(true, 0, 0, src.high(), false);
        self.byte(0xF7);
        self.modrm_rr(if signed { 7 } else { 6 }, src.0);
        self.note(
            s,
            format!("{} {}", if signed { "idiv" } else { "div" }, src.name()),
        );
    }

    /// shl/shr/sar dst, cl
    pub fn shift_cl(&mut self, op: Shift, dst: Reg) {
        let s = self.code.len();
        self.rex(true, 0, 0, dst.high(), false);
        self.byte(0xD3);
        self.modrm_rr(op as u8, dst.0);
        let name = match op {
            Shift::Shl => "shl",
            Shift::Shr => "shr",
            Shift::Sar => "sar",
        };
        self.note(s, format!("{name} {}, cl", dst.name()));
    }

    /// test a, b
    pub fn test(&mut self, a: Reg, b: Reg) {
        let s = self.code.len();
        self.rex(true, b.high(), 0, a.high(), false);
        self.byte(0x85);
        self.modrm_rr(b.0, a.0);
        self.note(s, format!("test {}, {}", a.name(), b.name()));
    }

    /// setcc dst8
    pub fn setcc(&mut self, cc: Cond, dst: Reg) {
        let s = self.code.len();
        self.rex(false, 0, 0, dst.high(), dst.0 >= 4);
        self.byte(0x0F);
        self.byte(0x90 + cc as u8);
        self.modrm_rr(0, dst.0);
        self.note(s, format!("set{} {}", cc.name(), dst.name8()));
    }

    /// and/or dst8, src8 (only al..bl)
    pub fn alu8(&mut self, op: Alu, dst: Reg, src: Reg) {
        let s = self.code.len();
        assert!(dst.0 < 4 && src.0 < 4);
        let opcode = match op {
            Alu::And => 0x20,
            Alu::Or => 0x08,
            _ => unimplemented!("8-bit {op:?}"),
        };
        self.byte(opcode);
        self.modrm_rr(src.0, dst.0);
        self.note(
            s,
            format!(
                "{} {}, {}",
                if op == Alu::And { "and" } else { "or" },
                dst.name8(),
                src.name8()
            ),
        );
    }

    /// movzx dst, src8
    pub fn movzx8(&mut self, dst: Reg, src: Reg) {
        let s = self.code.len();
        self.rex(true, dst.high(), 0, src.high(), false);
        self.byte(0x0F);
        self.byte(0xB6);
        self.modrm_rr(dst.0, src.0);
        self.note(s, format!("movzx {}, {}", dst.name(), src.name8()));
    }

    // -- SSE2 ------------------------------------------------------------------

    /// movsd xmm, [base + disp]
    pub fn load_sd(&mut self, dst: Xmm, base: Reg, disp: i32) {
        let s = self.code.len();
        self.byte(0xF2);
        self.rex(false, dst.high(), 0, base.high(), false);
        self.byte(0x0F);
        self.byte(0x10);
        self.modrm_mem(dst.0, base, disp);
        self.note(s, format!("movsd xmm{}, qword {}", dst.0, mem(base, disp)));
    }

    /// movsd [base + disp], xmm
    pub fn store_sd(&mut self, base: Reg, disp: i32, src: Xmm) {
        let s = self.code.len();
        self.byte(0xF2);
        self.rex(false, src.high(), 0, base.high(), false);
        self.byte(0x0F);
        self.byte(0x11);
        self.modrm_mem(src.0, base, disp);
        self.note(s, format!("movsd qword {}, xmm{}", mem(base, disp), src.0));
    }

    /// addsd/subsd/mulsd/divsd dst, src
    pub fn sse(&mut self, op: Sse, dst: Xmm, src: Xmm) {
        let s = self.code.len();
        self.byte(0xF2);
        self.rex(false, dst.high(), 0, src.high(), false);
        self.byte(0x0F);
        self.byte(op as u8);
        self.modrm_rr(dst.0, src.0);
        let name = match op {
            Sse::Add => "addsd",
            Sse::Mul => "mulsd",
            Sse::Sub => "subsd",
            Sse::Div => "divsd",
        };
        self.note(s, format!("{name} xmm{}, xmm{}", dst.0, src.0));
    }

    /// ucomisd a, b
    pub fn ucomisd(&mut self, a: Xmm, b: Xmm) {
        let s = self.code.len();
        self.byte(0x66);
        self.rex(false, a.high(), 0, b.high(), false);
        self.byte(0x0F);
        self.byte(0x2E);
        self.modrm_rr(a.0, b.0);
        self.note(s, format!("ucomisd xmm{}, xmm{}", a.0, b.0));
    }

    /// cvtsi2sd dst, src
    pub fn cvtsi2sd(&mut self, dst: Xmm, src: Reg) {
        let s = self.code.len();
        self.byte(0xF2);
        self.rex(true, dst.high(), 0, src.high(), false);
        self.byte(0x0F);
        self.byte(0x2A);
        self.modrm_rr(dst.0, src.0);
        self.note(s, format!("cvtsi2sd xmm{}, {}", dst.0, src.name()));
    }

    /// cvttsd2si dst, src
    pub fn cvttsd2si(&mut self, dst: Reg, src: Xmm) {
        let s = self.code.len();
        self.byte(0xF2);
        self.rex(true, dst.high(), 0, src.high(), false);
        self.byte(0x0F);
        self.byte(0x2C);
        self.modrm_rr(dst.0, src.0);
        self.note(s, format!("cvttsd2si {}, xmm{}", dst.name(), src.0));
    }

    // -- control flow ------------------------------------------------------------

    pub fn jmp(&mut self, l: Label) {
        let s = self.code.len();
        self.byte(0xE9);
        self.fixups.push((self.code.len(), l));
        self.i32(0);
        self.note(s, format!("jmp .L{}", l.0));
    }

    pub fn jcc(&mut self, cc: Cond, l: Label) {
        let s = self.code.len();
        self.byte(0x0F);
        self.byte(0x80 + cc as u8);
        self.fixups.push((self.code.len(), l));
        self.i32(0);
        self.note(s, format!("j{} .L{}", cc.name(), l.0));
    }

    /// call symbol (rel32)
    pub fn call(&mut self, symbol: &str) {
        let s = self.code.len();
        self.byte(0xE8);
        self.relocs.push(Reloc {
            offset: self.code.len(),
            symbol: symbol.to_string(),
        });
        self.i32(0);
        self.note(s, format!("call {symbol}"));
    }

    /// call [rip + slot] (through an import address table entry)
    pub fn call_indirect(&mut self, slot_symbol: &str) {
        let s = self.code.len();
        self.byte(0xFF);
        self.byte(0x15);
        self.relocs.push(Reloc {
            offset: self.code.len(),
            symbol: slot_symbol.to_string(),
        });
        self.i32(0);
        self.note(s, format!("call [rip + {slot_symbol}]"));
    }

    pub fn push_rbp(&mut self) {
        let s = self.code.len();
        self.byte(0x55);
        self.note(s, "push rbp".into());
    }

    pub fn pop_rbp(&mut self) {
        let s = self.code.len();
        self.byte(0x5D);
        self.note(s, "pop rbp".into());
    }

    /// sub rsp, imm32
    pub fn sub_rsp(&mut self, imm: i32) {
        let s = self.code.len();
        self.byte(0x48);
        self.byte(0x81);
        self.byte(0xEC);
        self.i32(imm);
        self.note(s, format!("sub rsp, {imm}"));
    }

    pub fn ret(&mut self) {
        let s = self.code.len();
        self.byte(0xC3);
        self.note(s, "ret".into());
    }

    pub fn ud2(&mut self) {
        let s = self.code.len();
        self.byte(0x0F);
        self.byte(0x0B);
        self.note(s, "ud2".into());
    }

    /// Render the listing with addresses relative to `base`.
    pub fn render_listing(&self, base: u64) -> String {
        let mut out = String::new();
        let mut entries = self.listing.iter().peekable();
        while let Some((off, text)) = entries.next() {
            if text.starts_with(".L") || text.starts_with(';') {
                writeln!(out, "{text}").unwrap();
                continue;
            }
            let end = entries
                .clone()
                .find(|(_, t)| !t.starts_with(".L") && !t.starts_with(';'))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(f: impl FnOnce(&mut Asm)) -> Vec<u8> {
        let mut a = Asm::default();
        f(&mut a);
        a.code
    }

    #[test]
    fn encodings() {
        // mov rax, [rbp - 8]
        assert_eq!(
            bytes(|a| a.load(RAX, RBP, -8)),
            vec![0x48, 0x8B, 0x85, 0xF8, 0xFF, 0xFF, 0xFF]
        );
        // mov [rsp + 32], rax
        assert_eq!(
            bytes(|a| a.store(RSP, 32, RAX)),
            vec![0x48, 0x89, 0x84, 0x24, 0x20, 0, 0, 0]
        );
        // mov r8, [rbp + 16]
        assert_eq!(
            bytes(|a| a.load(R8, RBP, 16)),
            vec![0x4C, 0x8B, 0x85, 0x10, 0, 0, 0]
        );
        // add rax, rcx
        assert_eq!(bytes(|a| a.alu(Alu::Add, RAX, RCX)), vec![0x48, 0x03, 0xC1]);
        // imul rax, rcx
        assert_eq!(bytes(|a| a.imul(RAX, RCX)), vec![0x48, 0x0F, 0xAF, 0xC1]);
        // idiv rcx
        assert_eq!(bytes(|a| a.div(RCX, true)), vec![0x48, 0xF7, 0xF9]);
        // shl rax, cl
        assert_eq!(
            bytes(|a| a.shift_cl(Shift::Shl, RAX)),
            vec![0x48, 0xD3, 0xE0]
        );
        // sete al ; setnp cl
        assert_eq!(bytes(|a| a.setcc(Cond::E, RAX)), vec![0x0F, 0x94, 0xC0]);
        assert_eq!(bytes(|a| a.setcc(Cond::NP, RCX)), vec![0x0F, 0x9B, 0xC1]);
        // and al, cl
        assert_eq!(bytes(|a| a.alu8(Alu::And, RAX, RCX)), vec![0x20, 0xC8]);
        // movzx rax, al
        assert_eq!(bytes(|a| a.movzx8(RAX, RAX)), vec![0x48, 0x0F, 0xB6, 0xC0]);
        // movsd xmm1, [rbp - 16]
        assert_eq!(
            bytes(|a| a.load_sd(Xmm(1), RBP, -16)),
            vec![0xF2, 0x0F, 0x10, 0x8D, 0xF0, 0xFF, 0xFF, 0xFF]
        );
        // addsd xmm0, xmm1
        assert_eq!(
            bytes(|a| a.sse(Sse::Add, Xmm(0), Xmm(1))),
            vec![0xF2, 0x0F, 0x58, 0xC1]
        );
        // ucomisd xmm0, xmm1
        assert_eq!(
            bytes(|a| a.ucomisd(Xmm(0), Xmm(1))),
            vec![0x66, 0x0F, 0x2E, 0xC1]
        );
        // cvtsi2sd xmm0, rax
        assert_eq!(
            bytes(|a| a.cvtsi2sd(Xmm(0), RAX)),
            vec![0xF2, 0x48, 0x0F, 0x2A, 0xC0]
        );
        // cvttsd2si rax, xmm0
        assert_eq!(
            bytes(|a| a.cvttsd2si(RAX, Xmm(0))),
            vec![0xF2, 0x48, 0x0F, 0x2C, 0xC0]
        );
        // mov byte [rax + 8], cl
        assert_eq!(
            bytes(|a| a.store_u8(RAX, 8, RCX)),
            vec![0x40, 0x88, 0x88, 8, 0, 0, 0]
        );
        // mov rax, -1 / mov rax, imm64
        assert_eq!(
            bytes(|a| a.mov_imm(RAX, -1)),
            vec![0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(
            bytes(|a| a.mov_imm(RAX, 1 << 40)),
            vec![0x48, 0xB8, 0, 0, 0, 0, 0, 1, 0, 0]
        );
        // mov r9, rax
        assert_eq!(bytes(|a| a.mov_rr(R9, RAX)), vec![0x49, 0x89, 0xC1]);
        // test rax, rax
        assert_eq!(bytes(|a| a.test(RAX, RAX)), vec![0x48, 0x85, 0xC0]);
    }
}
