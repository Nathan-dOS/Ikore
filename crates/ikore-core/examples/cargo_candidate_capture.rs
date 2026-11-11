//! Inspect qualified candidate capture without executing Cargo or project code.
//! Usage:
//! ```text
//! cargo run --locked --offline -p ikore-core --example cargo_candidate_capture
//!     -- PROJECT_ROOT [ADDITIONAL_INPUT_PATTERN ...]
//! ```

use std::path::PathBuf;

use ikore_core::orchestration::composition::validation::candidate::{
    capture_baseline, load_baseline, verify_candidate,
};

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let root = PathBuf::from(arguments.next().ok_or_else(|| {
        anyhow::anyhow!(
            "Usage: cargo_candidate_capture PROJECT_ROOT [ADDITIONAL_INPUT_PATTERN ...]"
        )
    })?);
    let additional_inputs: Vec<String> = arguments.collect();
    let temporary_root = std::env::temp_dir().canonicalize()?;
    anyhow::ensure!(
        !temporary_root.starts_with(root.canonicalize()?),
        "Configure the system temporary directory outside the project before this inspection."
    );
    let storage = tempfile::tempdir_in(temporary_root)?;
    let candidate = capture_baseline(&root, &storage.path().join("baseline"), &additional_inputs)?;
    let restored = load_baseline(&candidate.dir)?;
    verify_candidate(&restored)?;
    anyhow::ensure!(
        candidate == restored,
        "Restored capture differs from the baseline."
    );

    let mut total_bytes = 0u64;
    let mut opaque_files = 0usize;
    for path in restored.manifest.keys() {
        let bytes = std::fs::read(restored.dir.join(path))?;
        total_bytes += bytes.len() as u64;
        opaque_files += usize::from(bytes.contains(&0) || std::str::from_utf8(&bytes).is_err());
    }
    let count = |predicate: fn(&str) -> bool| {
        restored
            .manifest
            .keys()
            .filter(|path| predicate(path))
            .count()
    };
    let report = serde_json::json!({
        "schemaVersion": 1,
        "purpose": "candidate_capture_only",
        "restoredAndVerified": true,
        "candidateSha256": restored.candidate_sha256,
        "fileCount": restored.manifest.len(),
        "totalBytes": total_bytes,
        "opaqueFileCount": opaque_files,
        "baselineFileCount": restored.manifest.len(),
        "stagedTextFileCount": restored.staged_paths.len(),
        "cargoLockPresent": restored.manifest.contains_key("Cargo.lock"),
        "jsonlFileCount": count(|path| path.ends_with(".jsonl")),
        "srcBinFileCount": count(|path| path.starts_with("src/bin/") || path.contains("/src/bin/")),
        "icoFileCount": count(|path| path.ends_with(".ico")),
        "additionalPatternCount": additional_inputs.len(),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
