//! Generates Rust wire DTOs from the reviewed Protobuf source during the build.

use std::{
    error::Error,
    io::{self, Write},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proto_root = manifest_dir.join("../../../Contracts/proto");
    let common_types = proto_root.join("aurora/common/v1/types.proto");

    writeln!(
        io::stdout().lock(),
        "cargo:rerun-if-changed={}",
        common_types.display()
    )?;

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    config.compile_protos(&[common_types], &[proto_root])?;
    Ok(())
}
