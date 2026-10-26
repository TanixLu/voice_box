fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for symbol in ["DllCanUnloadNow", "DllGetClassObject"] {
            println!("cargo:rustc-link-arg-cdylib=/EXPORT:{symbol},PRIVATE");
        }
    }
}
