//! Linkers for the supported executable formats.
//!
//! The linker's job is the last step from "machine code" to "a file the OS
//! will run":
//!
//! 1. *Layout*: place all functions one after another, then the data, and
//!    decide every symbol's final address.
//! 2. *Relocation*: patch every placeholder the code generator left (see
//!    `codegen::Reloc`) with the real address or distance.
//! 3. *Format*: wrap everything in the headers the OS loader expects (PE
//!    on Windows, Mach-O on macOS). They tell the loader which bytes to map
//!    where, with which permissions, which libraries to load, and where
//!    execution starts.
//!
//! Usually a separate program (`ld`, `link.exe`) does this. Here the input
//! is always one whole program, so a small built-in linker is enough.

pub mod macho;
pub mod pe;
mod sha256;

use std::collections::HashMap;

/// A linked executable.
pub struct Image {
    pub bytes: Vec<u8>,
    /// Virtual address of each function, for listings.
    pub func_addrs: HashMap<String, u64>,
}
