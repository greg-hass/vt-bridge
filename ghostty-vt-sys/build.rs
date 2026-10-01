use std::{env, path::PathBuf};

// GHOSTTY_VT_DIR points at a `zig build` prefix (containing include/ and lib/).
fn main() {
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");
    let dir = env::var("GHOSTTY_VT_DIR").unwrap_or_else(|_| {
        format!("{}/Projects/ghostty-src/zig-out", env::var("HOME").unwrap())
    });
    println!("cargo:rustc-link-search=native={dir}/lib");
    // Static: no runtime library path to manage, and it ships inside the app binary.
    println!("cargo:rustc-link-lib=static=ghostty-vt");

    let bindings = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <ghostty/vt.h>")
        .clang_arg(format!("-I{dir}/include"))
        .allowlist_function("ghostty_.*")
        .allowlist_type("Ghostty.*")
        .allowlist_var("GHOSTTY_.*")
        .default_enum_style(bindgen::EnumVariation::ModuleConsts)
        .derive_default(true)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed");
    bindings
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs"))
        .unwrap();

    gen_keycodes(&dir);
}

// Map W3C KeyboardEvent.code strings ("KeyA", "ArrowUp") to GhosttyKey values by
// parsing the enum in key/event.h, so the table can't drift from the library.
fn gen_keycodes(dir: &str) {
    let src = std::fs::read_to_string(format!("{dir}/include/ghostty/vt/key/event.h")).unwrap();
    let mut out = String::from("pub static KEY_CODES: &[(&str, i32)] = &[\n");
    for line in src.lines() {
        let l = line.trim().trim_end_matches(',');
        let Some(name) = l.strip_prefix("GHOSTTY_KEY_") else { continue };
        if name.contains(' ') || name == "UNIDENTIFIED" || name == "MAX_VALUE" { continue; }
        let code = if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
            format!("Key{name}")
        } else {
            name.split('_')
                .map(|w| { let w = w.to_lowercase(); let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() })
                .collect::<String>()
        };
        out.push_str(&format!("    (\"{code}\", GhosttyKey::GHOSTTY_KEY_{name} as i32),\n"));
    }
    out.push_str("];\n");
    std::fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("keycodes.rs"), out).unwrap();
}
