#[path = "build_support/schema_version.rs"]
mod schema_version;

const PROTO_ROOT: &str = "contracts/proto";
const MANIFEST_PROTO: &str = "kemana/bundle/v1/manifest.proto";
const BUNDLE_SCHEMA: &str = "contracts/bundle/schema.sql";
const SCHEMA_VERSION_FILE: &str = "bundle_schema_version.rs";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = format!("{PROTO_ROOT}/{MANIFEST_PROTO}");
    if !std::path::Path::new(&manifest_path).exists() {
        return Err(format!(
            "{manifest_path} is missing; run `git submodule update --init` to fetch the contracts"
        )
        .into());
    }
    println!("cargo:rerun-if-changed={manifest_path}");
    println!("cargo:rerun-if-changed={BUNDLE_SCHEMA}");
    println!("cargo:rerun-if-changed=build_support/schema_version.rs");

    let schema_sql = std::fs::read_to_string(BUNDLE_SCHEMA)
        .map_err(|error| format!("cannot read {BUNDLE_SCHEMA}: {error}"))?;
    let version = schema_version::parse_schema_version(&schema_sql)
        .map_err(|error| format!("{BUNDLE_SCHEMA}: {error}"))?;
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(
        out_dir.join(SCHEMA_VERSION_FILE),
        format!("{version}_u32\n"),
    )?;

    // protox compiles the proto in Rust, so neither developers nor CI need protoc installed.
    let descriptors = protox::compile([MANIFEST_PROTO], [PROTO_ROOT])?;
    prost_build::Config::new().compile_fds(descriptors)?;
    Ok(())
}
