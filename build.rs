fn main() {
    capnpc::CompilerCommand::new()
        .file("tunnelrpc.capnp")
        .file("quic_metadata_protocol.capnp")
        .run()
        .expect("schema compiler command");
}
