fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    // The ytdlp feature carries a JavaScript engine for YouTube's challenges (src/ytdlp/js.rs).
    #[cfg(feature = "ytdlp")]
    {
        println!("cargo:rerun-if-changed=src/ytdlp/js.c");
        println!("cargo:rerun-if-changed=third_party/quickjs");
        cc::Build::new()
            .file("third_party/quickjs/quickjs-amalgam.c")
            .file("src/ytdlp/js.c")
            .include("third_party/quickjs")
            .define("_GNU_SOURCE", None)
            // Optimized even in debug builds: solving a challenge at -O0 takes far too long.
            .opt_level(2)
            .warnings(false)
            .compile("elsewhere_quickjs");
    }
}
