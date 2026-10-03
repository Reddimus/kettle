mod build_support;

fn main() {
    for path in [
        "locales/schema.toml",
        "locales/en.toml",
        "locales/es.toml",
        "build_support.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let read = |path| std::fs::read_to_string(path).expect("read catalogue input");
    let schema = read("locales/schema.toml");
    let en = read("locales/en.toml");
    let es = read("locales/es.toml");
    let generated = build_support::generate(&schema, &en, &es)
        .unwrap_or_else(|error| panic!("kettle-i18n catalogue: {error}"));
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    std::fs::write(out.join("catalogue.rs"), generated).expect("write generated catalogue");
}
