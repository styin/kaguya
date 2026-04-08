/// Compiles the proto files to Rust code using tonic_build. 
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure that PROTOC is available in the environment.

    tonic_build::configure()
        .build_server(true)   // Gateway as server: ListenerService, RouterControlService
        .build_client(true)   // Gateway as client: TalkerService, ReasonerService
        .compile_protos(
            &["../proto/kaguya/v1/kaguya.proto"],
            &["../proto"],
        )?;
    Ok(())
}