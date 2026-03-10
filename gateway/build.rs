use std::env;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env::set_var("PROTOC", r"D:\protoc-34.0-win64\bin\protoc.exe");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                "../proto/kaguya/listener.proto",
                "../proto/kaguya/talker.proto",
                "../proto/kaguya/reasoner.proto",
                "../proto/kaguya/gateway.proto",
            ],
            &["../proto"],
        )?;
    Ok(())
}