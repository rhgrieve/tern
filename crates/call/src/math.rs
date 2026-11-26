//! The math functions that glibc 2.43 and 2.44 versioned anew, at the version every glibc of x86-64
//! has. build.rs sends the program's calls here with `--wrap`. All in one piece of assembly: a
//! `.symver` only holds within its own object.

#[cfg(all(target_env = "gnu", target_arch = "x86_64"))]
std::arch::global_asm!(
    ".macro old name",
    ".globl __wrap_\\name",
    ".type __wrap_\\name, @function",
    "__wrap_\\name:",
    "jmp tern_old_\\name@PLT",
    ".symver tern_old_\\name, \\name@GLIBC_2.2.5",
    ".endm",
    "old acosf",
    "old asinf",
    "old atan2f",
    "old log10f",
    "old sinhf",
    "old coshf",
    "old acoshf",
    "old sinh",
    "old cosh",
    ".purgem old",
);
