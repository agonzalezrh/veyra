fn main() {
    // Build stamp (H2 field diagnostics): a stale binary announces
    // itself in the startup log instead of silently missing fixes.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=VEYRA_BUILD_TS={ts}");
    println!(
        "cargo:rustc-env=VEYRA_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );

    // Tell cargo to link against liblgmp
    println!("cargo:rustc-link-lib=lgmp");
    println!("cargo:rustc-link-search=/usr/local/lib");
    // Rebuild if the library changes
    println!("cargo:rerun-if-changed=/usr/local/lib/liblgmp.a");
}
