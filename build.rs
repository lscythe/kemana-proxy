const PROTO_ROOT: &str = "contracts/proto";
const MANIFEST_PROTO: &str = "kemana/bundle/v1/manifest.proto";
const BUNDLE_SCHEMA: &str = "contracts/bundle/schema.sql";

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

    // protox compiles the proto in Rust, so neither developers nor CI need protoc installed.
    let descriptors = protox::compile([MANIFEST_PROTO], [PROTO_ROOT])?;
    prost_build::Config::new().compile_fds(descriptors)?;
    Ok(())
}
