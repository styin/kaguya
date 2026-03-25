fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    if std::env::var("PROTOC").is_err() {
        // 改成你自己的 protoc 路径
        std::env::set_var("PROTOC", r"D:\protoc-34.0-win64\bin\protoc.exe");
    }

    tonic_build::configure()
        .build_server(true)   // Gateway 作为 server: ListenerService, RouterControlService
        .build_client(true)   // Gateway 作为 client: TalkerService, ReasonerService
        .compile_protos(
            &["../proto/kaguya/v1/kaguya.proto"],
            &["../proto"],
        )?;
    Ok(())
}