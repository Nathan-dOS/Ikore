//! ADR-037 M6: `review_finding` and `claim_under_verification` are reserved
//! for the multi-reviewer feature. They exist in the contract, and no crate
//! produces them before that feature defines its reviewer contract.

use std::path::{Path, PathBuf};

use ikore_contracts::exchange::ExchangeKind;

fn rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

#[test]
fn reserved_review_kinds_exist_and_nothing_produces_them() {
    let reserved = ExchangeKind::ALL
        .into_iter()
        .filter(|kind| kind.is_reserved())
        .map(ExchangeKind::as_str)
        .collect::<Vec<_>>();
    assert_eq!(reserved, ["review_finding", "claim_under_verification"]);

    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory");
    let contract = crates
        .join("ikore-contracts")
        .join("src")
        .join("exchange.rs");
    let needles = ["ReviewFinding", "ClaimUnderVerification"]
        .into_iter()
        .map(|variant| format!("ExchangeKind::{variant}"))
        .chain(reserved.iter().map(|name| format!("\"{name}\"")))
        .collect::<Vec<_>>();
    let mut sources = Vec::new();
    rust_sources(crates, &mut sources);
    assert!(sources.len() > 100, "the scan found the workspace sources");
    let producers = sources
        .iter()
        .filter(|path| {
            **path != contract
                && path.file_name() != Some(std::ffi::OsStr::new("reserved_exchange_kinds.rs"))
        })
        .filter(|path| {
            let text = std::fs::read_to_string(path).expect("read a source file");
            needles.iter().any(|needle| text.contains(needle.as_str()))
        })
        .collect::<Vec<_>>();
    assert!(
        producers.is_empty(),
        "reserved review kinds used before the multi-reviewer feature: {producers:?}"
    );
}
