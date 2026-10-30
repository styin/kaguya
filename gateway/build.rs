/// Compiles the proto files to Rust code using tonic_build.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_build::configure()
        .build_server(true) // RouterControlService and test services
        .build_client(true) // TalkerService, ListenerService, ReasonerService
        .compile_protos(&["../proto/kaguya/v1/kaguya.proto"], &["../proto"])?;
    Ok(())
}
