//! glibc 2.43 and 2.44 gave these math functions new versions, and a program links the newest it
//! sees. Each call goes instead to the version every glibc has, so that tern-call starts where
//! Tern does (see src/math.rs).
fn main() {
    let gnu = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    let x86 = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64");
    if gnu && x86 {
        for name in [
            "acosf", "asinf", "atan2f", "log10f", "sinhf", "coshf", "acoshf", "sinh", "cosh",
        ] {
            println!("cargo:rustc-link-arg-bins=-Wl,--wrap={name}");
        }
    }
}
