// Match the application's prebuilt linking strategy without depending on Tauri.
fn main() {
    println!("cargo:rerun-if-env-changed=DUCKDB_LIB_DIR");
    println!("cargo:rerun-if-env-changed=DUCKDB_STATIC");
    if std::env::var_os("CARGO_FEATURE_SOURCE_BUNDLED").is_some() {
        return;
    }
    let dir = std::env::var("DUCKDB_LIB_DIR")
        .expect("Run through node scripts/with-duckdb.mjs to stage static archives");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|name| {
            name.strip_prefix("lib")
                .and_then(|name| name.strip_suffix(".a"))
                .map(str::to_owned)
        })
        .collect();
    names.sort_by_key(|name| match name.as_str() {
        "duckdb_static" => 0,
        "duckdb_generated_extension_loader" => 1,
        _ => 2,
    });
    println!("cargo:rustc-link-search=native={dir}");
    for name in names {
        println!("cargo:rustc-link-lib=static={name}");
    }
    match std::env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "macos" => println!("cargo:rustc-link-lib=c++"),
        "linux" => {
            for name in ["stdc++", "pthread", "dl", "m"] {
                println!("cargo:rustc-link-lib={name}");
            }
        }
        _ => {}
    }
}
