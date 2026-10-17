//! Exact edit application (SC-009).
//!
//! The host reconstructs every destination from the immutable original bytes
//! and the worker's operations. There is no fuzzy matching, no relocation, no
//! line-number guessing: an anchor is an exact byte interval issued for an
//! exact source hash, and a replacement either fits it or is rejected.

use std::collections::BTreeMap;

use super::{
    AnchorBindingV1, CompositionError, EditEntryV1, EditOperationV1, ErrorCode, Handle, Invariant,
    MAX_ARTIFACT_BYTES, MAX_REPLACE_FILE_BYTES, Sha256Hex,
};

fn reject(code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc009, code, message)
}

/// The complete reconstructed destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconstructed {
    pub content: String,
    pub sha256: Sha256Hex,
}

impl Reconstructed {
    fn new(content: String) -> Result<Self, CompositionError> {
        if content.len() > MAX_ARTIFACT_BYTES {
            return Err(reject(
                ErrorCode::CapacityExceeded,
                "The reconstructed file exceeds the 1 MiB artifact limit.",
            ));
        }
        if content.as_bytes().contains(&0) {
            return Err(reject(
                ErrorCode::UnsupportedOperation,
                "The reconstructed file contains a NUL byte.",
            ));
        }
        let sha256 = Sha256Hex::of(content.as_bytes());
        Ok(Self { content, sha256 })
    }
}

/// The edits of one payload grouped by the destination they reconstruct, with
/// the artifact key the worker assigned to that destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationEdits<'a> {
    pub destination: Handle,
    pub artifact_key: Handle,
    pub operations: Vec<&'a EditOperationV1>,
}

/// Groups edits by destination. `anchor_source` resolves an anchor handle to
/// the source it was issued for. Enforces the artifact-key rule: one key per
/// destination, distinct keys for distinct destinations.
pub fn group_by_destination<'a>(
    edits: &'a [EditEntryV1],
    anchor_source: impl Fn(&Handle) -> Option<&'a Handle>,
) -> Result<Vec<DestinationEdits<'a>>, CompositionError> {
    let mut groups: BTreeMap<Handle, DestinationEdits<'a>> = BTreeMap::new();
    let mut key_owner: BTreeMap<Handle, Handle> = BTreeMap::new();
    for entry in edits {
        let destination = match &entry.edit {
            EditOperationV1::CreateFile {
                destination_ref, ..
            } => destination_ref.clone(),
            EditOperationV1::ReplaceFile { source_ref, .. } => source_ref.clone(),
            EditOperationV1::ReplaceAnchor { anchor_ref, .. } => {
                anchor_source(anchor_ref).cloned().ok_or_else(|| {
                    reject(
                        ErrorCode::ReferenceConflict,
                        format!("Anchor `{anchor_ref}` was not issued for this unit."),
                    )
                    .with_related([anchor_ref.clone()])
                })?
            }
        };
        if let Some(owner) = key_owner.get(&entry.artifact_key)
            && owner != &destination
        {
            return Err(reject(
                ErrorCode::InvalidShape,
                format!(
                    "artifactKey `{}` is used for two destinations (`{owner}` and `{destination}`).",
                    entry.artifact_key
                ),
            ));
        }
        key_owner.insert(entry.artifact_key.clone(), destination.clone());
        let group = groups
            .entry(destination.clone())
            .or_insert_with(|| DestinationEdits {
                destination: destination.clone(),
                artifact_key: entry.artifact_key.clone(),
                operations: Vec::new(),
            });
        if group.artifact_key != entry.artifact_key {
            return Err(reject(
                ErrorCode::InvalidShape,
                format!(
                    "Destination `{destination}` receives edits under two artifact keys (`{}` and `{}`).",
                    group.artifact_key, entry.artifact_key
                ),
            ));
        }
        group.operations.push(&entry.edit);
    }
    Ok(groups.into_values().collect())
}

/// Reconstructs one destination. `original` is the exact current bytes of an
/// existing/candidate source, or `None` for a planned destination. `anchors`
/// are the bindings issued for that source.
pub fn apply_destination_edits(
    original: Option<&[u8]>,
    anchors: &[AnchorBindingV1],
    operations: &[&EditOperationV1],
) -> Result<Reconstructed, CompositionError> {
    let Some(first) = operations.first() else {
        return Err(reject(
            ErrorCode::InvalidShape,
            "No edit was supplied for the destination.",
        ));
    };
    match first {
        EditOperationV1::CreateFile { content, .. } => {
            if operations.len() != 1 {
                if operations
                    .iter()
                    .filter(|operation| matches!(operation, EditOperationV1::CreateFile { .. }))
                    .count()
                    > 1
                {
                    return Err(reject(
                        ErrorCode::ReferenceConflict,
                        "Multiple `create_file` edits target the same destination; the host cannot choose between their contents.",
                    ));
                }
                return Err(reject(
                    ErrorCode::UnsupportedOperation,
                    "`create_file` must be the only operation for its destination.",
                ));
            }
            if original.is_some() {
                return Err(reject(
                    ErrorCode::StaleInput,
                    "`create_file` targets a destination that already exists.",
                ));
            }
            Reconstructed::new(content.clone())
        }
        EditOperationV1::ReplaceFile { content, .. } => {
            if operations.len() != 1 {
                return Err(reject(
                    ErrorCode::UnsupportedOperation,
                    "`replace_file` must be the only operation for its destination.",
                ));
            }
            let Some(original) = original else {
                return Err(reject(
                    ErrorCode::UnsupportedOperation,
                    "`replace_file` requires an existing source; use `create_file` for a planned destination.",
                ));
            };
            if original.len() > MAX_REPLACE_FILE_BYTES || content.len() > MAX_REPLACE_FILE_BYTES {
                return Err(reject(
                    ErrorCode::UnsupportedOperation,
                    "`replace_file` is limited to sources and replacements of at most 8 KiB; use anchors for larger files.",
                ));
            }
            if content.is_empty() {
                return Err(reject(
                    ErrorCode::UnsupportedOperation,
                    "Emptying an existing file is not supported by this protocol.",
                ));
            }
            Reconstructed::new(content.clone())
        }
        EditOperationV1::ReplaceAnchor { .. } => {
            let Some(original) = original else {
                return Err(reject(
                    ErrorCode::UnsupportedOperation,
                    "`replace_anchor` requires an existing source.",
                ));
            };
            apply_anchor_replacements(original, anchors, operations)
        }
    }
}

fn apply_anchor_replacements(
    original: &[u8],
    anchors: &[AnchorBindingV1],
    operations: &[&EditOperationV1],
) -> Result<Reconstructed, CompositionError> {
    let source_hash = Sha256Hex::of(original);
    let text = std::str::from_utf8(original)
        .map_err(|_| reject(ErrorCode::UnsupportedOperation, "The source is not UTF-8."))?;
    // Resolve and validate every operation before touching any byte.
    let mut replacements: Vec<(usize, usize, &str, &Handle)> = Vec::with_capacity(operations.len());
    for operation in operations {
        let EditOperationV1::ReplaceAnchor {
            anchor_ref,
            content,
        } = operation
        else {
            return Err(reject(
                ErrorCode::UnsupportedOperation,
                "Anchor replacements cannot be mixed with whole-file operations on one destination.",
            ));
        };
        let anchor = anchors
            .iter()
            .find(|anchor| &anchor.anchor_ref == anchor_ref)
            .ok_or_else(|| {
                reject(
                    ErrorCode::ReferenceConflict,
                    format!("Anchor `{anchor_ref}` was not issued for this source."),
                )
                .with_related([anchor_ref.clone()])
            })?;
        if anchor.source_sha256 != source_hash {
            return Err(reject(
                ErrorCode::StaleInput,
                format!("Anchor `{anchor_ref}` was issued for a different version of the source."),
            )
            .with_related([anchor_ref.clone()]));
        }
        let (start, end) = (anchor.start_byte as usize, anchor.end_byte as usize);
        if start > end
            || end > original.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            return Err(reject(
                ErrorCode::StaleInput,
                format!("Anchor `{anchor_ref}` does not fit the source."),
            )
            .with_related([anchor_ref.clone()]));
        }
        if Sha256Hex::of(&original[start..end]) != anchor.region_sha256 {
            return Err(reject(
                ErrorCode::StaleInput,
                format!("Anchor `{anchor_ref}` region bytes changed."),
            )
            .with_related([anchor_ref.clone()]));
        }
        if replacements
            .iter()
            .any(|(_, _, _, used)| *used == anchor_ref)
        {
            return Err(reject(
                ErrorCode::UnsupportedOperation,
                format!("Anchor `{anchor_ref}` is replaced twice."),
            )
            .with_related([anchor_ref.clone()]));
        }
        replacements.push((start, end, content.as_str(), anchor_ref));
    }
    replacements.sort_by_key(|(start, end, _, _)| (*start, *end));
    for pair in replacements.windows(2) {
        let (_, previous_end, _, previous) = pair[0];
        let (next_start, _, _, next) = pair[1];
        if next_start < previous_end {
            return Err(reject(
                ErrorCode::UnsupportedOperation,
                format!("Anchors `{previous}` and `{next}` overlap."),
            )
            .with_related([previous.clone(), next.clone()]));
        }
    }
    // Apply from the highest offset downward so earlier offsets stay valid.
    let mut result = text.to_owned();
    for (start, end, content, _) in replacements.iter().rev() {
        result.replace_range(*start..*end, content);
    }
    if result.is_empty() && !original.is_empty() {
        return Err(reject(
            ErrorCode::UnsupportedOperation,
            "Anchor replacements may not empty an existing file; deletion is not supported.",
        ));
    }
    Reconstructed::new(result)
}

#[cfg(test)]
mod tests {
    use super::super::references::ReferenceTable;
    use super::super::{RelPath, SourceRole};
    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn issue(bytes: &[u8], lines: &[(u32, u32)]) -> (Handle, Vec<AnchorBindingV1>) {
        let mut builder = ReferenceTable::builder();
        let src = builder
            .add_existing(
                RelPath::new("src/lib.rs").unwrap(),
                Sha256Hex::of(bytes),
                SourceRole::Source,
            )
            .unwrap();
        for (start, end) in lines {
            builder.add_line_anchor(&src, bytes, *start, *end).unwrap();
        }
        (src, builder.build().anchors().to_vec())
    }

    fn anchor_edit(name: &str, content: &str) -> EditOperationV1 {
        EditOperationV1::ReplaceAnchor {
            anchor_ref: handle(name),
            content: content.into(),
        }
    }

    #[test]
    fn anchor_replacements_reconstruct_exact_bytes_and_preserve_the_rest() {
        let bytes = "fn a() {}\r\nfn é() { old }\nfn c() {}\n".as_bytes();
        let (_, anchors) = issue(bytes, &[(2, 2), (3, 3)]);
        let edits = [
            anchor_edit("anchor_2", "fn c() { new }\n"),
            anchor_edit("anchor_1", "fn é() { new }\n"),
        ];
        let refs = edits.iter().collect::<Vec<_>>();
        let out = apply_destination_edits(Some(bytes), &anchors, &refs).unwrap();
        assert_eq!(out.content, "fn a() {}\r\nfn é() { new }\nfn c() { new }\n");
        assert_eq!(out.sha256, Sha256Hex::of(out.content.as_bytes()));
        // Removing an interval is an empty replacement; the other bytes are identical.
        let removal = [anchor_edit("anchor_1", "")];
        let refs = removal.iter().collect::<Vec<_>>();
        let out = apply_destination_edits(Some(bytes), &anchors, &refs).unwrap();
        assert_eq!(out.content, "fn a() {}\r\nfn c() {}\n");
    }

    #[test]
    fn stale_overlapping_unknown_and_emptying_anchors_are_rejected() {
        let bytes = b"line one\nline two\nline three\n";
        let (_, anchors) = issue(bytes, &[(1, 2), (2, 3), (1, 3)]);
        let single = [anchor_edit("anchor_1", "x\n")];
        let refs = single.iter().collect::<Vec<_>>();
        // Different source bytes than the anchor was issued for.
        let error =
            apply_destination_edits(Some(b"line one\nline TWO\nline three\n"), &anchors, &refs)
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleInput);
        // Overlap between anchor_1 (lines 1-2) and anchor_2 (lines 2-3).
        let overlapping = [
            anchor_edit("anchor_1", "x\n"),
            anchor_edit("anchor_2", "y\n"),
        ];
        let refs = overlapping.iter().collect::<Vec<_>>();
        assert_eq!(
            apply_destination_edits(Some(bytes), &anchors, &refs)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedOperation
        );
        // Unknown anchor and duplicate use.
        let unknown = [anchor_edit("anchor_9", "x\n")];
        let refs = unknown.iter().collect::<Vec<_>>();
        assert_eq!(
            apply_destination_edits(Some(bytes), &anchors, &refs)
                .unwrap_err()
                .code,
            ErrorCode::ReferenceConflict
        );
        let twice = [
            anchor_edit("anchor_1", "x\n"),
            anchor_edit("anchor_1", "y\n"),
        ];
        let refs = twice.iter().collect::<Vec<_>>();
        assert!(apply_destination_edits(Some(bytes), &anchors, &refs).is_err());
        // Emptying the whole file through the whole-file anchor is refused.
        let empty = [anchor_edit("anchor_3", "")];
        let refs = empty.iter().collect::<Vec<_>>();
        assert_eq!(
            apply_destination_edits(Some(bytes), &anchors, &refs)
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedOperation
        );
        // Anchors need an existing source.
        assert!(apply_destination_edits(None, &anchors, &refs).is_err());
    }

    #[test]
    fn whole_file_operations_follow_the_size_and_existence_rules() {
        let create = EditOperationV1::CreateFile {
            destination_ref: handle("plan_1"),
            content: "// new\n".into(),
        };
        let out = apply_destination_edits(None, &[], &[&create]).unwrap();
        assert_eq!(out.content, "// new\n");
        let conflicting_create = EditOperationV1::CreateFile {
            destination_ref: handle("plan_1"),
            content: "// conflicting\n".into(),
        };
        let conflict =
            apply_destination_edits(None, &[], &[&create, &conflicting_create]).unwrap_err();
        assert_eq!(conflict.code, ErrorCode::ReferenceConflict);
        assert!(
            conflict
                .message
                .contains("cannot choose between their contents")
        );
        assert_eq!(
            apply_destination_edits(Some(b"exists"), &[], &[&create])
                .unwrap_err()
                .code,
            ErrorCode::StaleInput
        );
        let replace = EditOperationV1::ReplaceFile {
            source_ref: handle("src_1"),
            content: "fn v() {}\n".into(),
        };
        assert_eq!(
            apply_destination_edits(Some(b"fn v() { 1 }\n"), &[], &[&replace])
                .unwrap()
                .content,
            "fn v() {}\n"
        );
        assert!(apply_destination_edits(None, &[], &[&replace]).is_err());
        let large = vec![b'x'; MAX_REPLACE_FILE_BYTES + 1];
        assert_eq!(
            apply_destination_edits(Some(&large), &[], &[&replace])
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedOperation
        );
        let big_replacement = EditOperationV1::ReplaceFile {
            source_ref: handle("src_1"),
            content: "x".repeat(MAX_REPLACE_FILE_BYTES + 1),
        };
        assert!(apply_destination_edits(Some(b"small"), &[], &[&big_replacement]).is_err());
        // Two whole-file operations, or mixing with anchors, is unsupported.
        assert!(apply_destination_edits(Some(b"small"), &[], &[&replace, &replace]).is_err());
        let anchor = anchor_edit("anchor_1", "");
        assert!(apply_destination_edits(Some(b"small"), &[], &[&anchor, &replace]).is_err());
        let nul = EditOperationV1::CreateFile {
            destination_ref: handle("plan_1"),
            content: "a\0b".into(),
        };
        assert!(apply_destination_edits(None, &[], &[&nul]).is_err());
    }

    #[test]
    fn grouping_enforces_one_artifact_key_per_destination() {
        let entries = vec![
            EditEntryV1 {
                artifact_key: handle("lib"),
                edit: anchor_edit("anchor_1", "a"),
            },
            EditEntryV1 {
                artifact_key: handle("lib"),
                edit: anchor_edit("anchor_2", "b"),
            },
            EditEntryV1 {
                artifact_key: handle("new"),
                edit: EditOperationV1::CreateFile {
                    destination_ref: handle("plan_1"),
                    content: "c".into(),
                },
            },
        ];
        let src = handle("src_1");
        let resolve = |anchor: &Handle| (anchor.as_str().starts_with("anchor_")).then_some(&src);
        let groups = group_by_destination(&entries, resolve).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].destination, handle("plan_1"));
        assert_eq!(groups[1].destination, src);
        assert_eq!(groups[1].operations.len(), 2);
        // Same key for two destinations.
        let mut bad = entries.clone();
        bad[2].artifact_key = handle("lib");
        assert!(group_by_destination(&bad, resolve).is_err());
        // Two keys for one destination.
        let mut bad = entries.clone();
        bad[1].artifact_key = handle("other");
        assert!(group_by_destination(&bad, resolve).is_err());
        // Unknown anchor.
        assert!(group_by_destination(&entries, |_| None).is_err());
    }
}
