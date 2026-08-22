fn main() {
    // ggml-metal's __builtin_available checks need clang's compiler runtime
    // (___isPlatformVersionAtLeast); rustc links with cc but not clang_rt, so
    // release links fail without this.
    #[cfg(target_os = "macos")]
    {
        let resource_dir = std::process::Command::new("xcrun")
            .args(["clang", "--print-resource-dir"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());
        if let Some(dir) = resource_dir {
            println!("cargo:rustc-link-search=native={dir}/lib/darwin");
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        // AVCaptureDevice (mic permission status + request) needs the framework
        // actually loaded; nothing else pulls it in.
        println!("cargo:rustc-link-lib=framework=AVFoundation");
    }
    tauri_build::build()
}
