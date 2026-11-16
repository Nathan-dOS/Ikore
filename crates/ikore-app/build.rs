fn main() {
    println!("cargo:rerun-if-changed=ikore.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    embed_resource::compile("ikore.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed the Ikore Win32 resources");
}
