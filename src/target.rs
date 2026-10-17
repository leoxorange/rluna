//! Compilation targets: an instruction set, an executable format and the
//! operating system layer of the runtime.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// x86-64 code in a PE `.exe`, importing from kernel32.dll.
    WindowsX64,
    /// AArch64 code in a signed Mach-O executable, importing from libSystem.
    MacosArm64,
}

impl Target {
    pub const ALL: [Target; 2] = [Target::WindowsX64, Target::MacosArm64];

    /// The target matching the machine the compiler runs on, if supported.
    pub fn host() -> Option<Target> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Target::WindowsX64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Target::MacosArm64)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Target::WindowsX64 => "windows-x64",
            Target::MacosArm64 => "macos-arm64",
        }
    }

    pub fn from_name(name: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.name() == name)
    }

    /// File extension of executables, without the dot (may be empty).
    pub fn exe_extension(self) -> &'static str {
        match self {
            Target::WindowsX64 => "exe",
            Target::MacosArm64 => "",
        }
    }

    /// The operating system layer of the runtime (see `runtime/core.lir`).
    pub fn runtime_os_lir(self) -> &'static str {
        match self {
            Target::WindowsX64 => include_str!("runtime/windows.lir"),
            Target::MacosArm64 => include_str!("runtime/macos.lir"),
        }
    }
}

/// The host target, or Windows x64 on machines no target runs on.
impl Default for Target {
    fn default() -> Target {
        Target::host().unwrap_or(Target::WindowsX64)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
