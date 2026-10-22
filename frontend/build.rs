fn main() {
    translations();
    #[cfg(feature = "desktop")]
    {
        // Rebuild native resources and generated window/tray images on icon-only changes.
        for icon in ["icons/icon.png", "icons/icon.ico", "icons/icon.icns"] {
            println!("cargo:rerun-if-changed={icon}");
        }
        tauri_build::build();
    }
}

// English source text is the catalog key and fallback. Generate ordinary Rust
// matches and format! calls so both targets share a catalog without runtime I/O.
fn translations() {
    use std::{collections::BTreeMap, env, fmt::Write, fs, path::PathBuf};
    println!("cargo:rerun-if-changed=locales/tr.json");
    let catalog: BTreeMap<String, String> =
        serde_json::from_str(&fs::read_to_string("locales/tr.json").unwrap()).unwrap();
    let mut code = String::from(
        "pub fn translate(language: Language, source: &str) -> &str {\n\
         if language == Language::English { return source; }\nmatch source {\n",
    );
    for (en, tr) in &catalog {
        assert!(!tr.is_empty(), "empty translation for {en}");
        writeln!(code, "{en:?} => {tr:?},").unwrap();
    }
    code.push_str("_ => source,\n}}\n#[cfg(test)] const CATALOG: &[(&str, &str)] = &[\n");
    for (en, tr) in &catalog {
        writeln!(code, "({en:?}, {tr:?}),").unwrap();
    }
    code.push_str("] ;\n#[macro_export]\nmacro_rules! localized_format {\n");
    for (en, tr) in &catalog {
        if en.contains('{') {
            // Explicit named arguments at call sites preserve macro hygiene.
            writeln!(
                code,
                "($language:expr, {en:?} $(, $($args:tt)*)?) => {{{{\n\
                 match $language {{\n\
                 $crate::localization::Language::English => format!({en:?} $(, $($args)*)?),\n\
                 $crate::localization::Language::Turkish => format!({tr:?} $(, $($args)*)?),\n\
                 }}\n}}}};"
            )
            .unwrap();
        }
    }
    code.push_str("}\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("translations.rs"),
        code,
    )
    .unwrap();
}
