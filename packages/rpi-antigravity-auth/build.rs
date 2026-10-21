fn main() {
    println!("cargo:rerun-if-changed=../../vendor/private-files/native/addon.c");
    println!("cargo:rerun-if-changed=../../vendor/private-files/native/open-policy.h");
    println!("cargo:rerun-if-changed=../../vendor/private-files/legacy/native/addon.c");
    println!("cargo:rerun-if-changed=../../vendor/private-files/legacy/native/open-policy.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        cc::Build::new()
            .file("../../vendor/private-files/native/addon.c")
            .file("../../vendor/private-files/legacy/native/addon.c")
            .define("RPI_NATIVE", None)
            .flag_if_supported("/std:c11")
            .compile("rpi_private_files");
        println!("cargo:rustc-link-lib=advapi32");
        println!("cargo:rustc-link-lib=ntdll");
    }
}
