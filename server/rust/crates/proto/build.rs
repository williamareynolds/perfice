fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The .proto lives with the Go services; it is the shared contract between
    // the two implementations and must not be duplicated.
    let proto = "../../../proto/auth.proto";
    println!("cargo:rerun-if-changed={proto}");
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto], &["../../../proto"])?;
    Ok(())
}
