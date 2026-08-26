fn main() {
    println!("cargo:rerun-if-changed=hard-vibe-code.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    embed_resource::compile("hard-vibe-code.rc", embed_resource::NONE)
        .manifest_required()
        .expect("falha ao incorporar os recursos Win32 do Hard Vibe Code");
}
