// Protobuf types for Blizzard's s2client-proto (proto/, MIT). Compiled with
// protox so no `protoc` install is needed.
fn main() {
    println!("cargo:rerun-if-changed=proto");
    let fds = protox::compile(["s2clientprotocol/sc2api.proto"], ["proto"]).expect("compile s2client protos");
    prost_build::Config::new().compile_fds(fds).expect("generate s2client types");
}
