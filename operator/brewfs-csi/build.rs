fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/csi.proto");
    tonic_prost_build::configure()
        .build_server(true)
        .compile_protos(&["proto/csi.proto"], &["proto", "/usr/include"])?;
    Ok(())
}
