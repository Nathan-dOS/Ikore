//! Host-only publication. No model tool writes and no implicit trust of a path.
use super::{
    ContentSha256, ExpectedFileContent, PublicationReplayDecision, RelativeSourcePath,
    publication_replay_decision,
};
use crate::persistence_service::PersistenceServiceError as Error;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024;

fn forbidden(message: &str) -> Error {
    Error::Validation(message.to_owned())
}

fn reject_link(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(forbidden(
                "Publication does not allow junctions or reparse points.",
            ));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(forbidden("Publication does not allow symlinks."));
    }
    Ok(())
}

pub(crate) fn safe_target(
    root: &Path,
    relative: &str,
    create_parents: bool,
) -> Result<PathBuf, Error> {
    let relative = RelativeSourcePath::new(relative).map_err(|e| forbidden(&e.to_string()))?;
    // Check every existing ancestor of the root too (canonicalize alone follows junctions).
    for ancestor in root.ancestors() {
        if ancestor.exists() {
            reject_link(ancestor)?;
        }
    }
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(forbidden("Invalid workspace."));
    }
    let segments: Vec<_> = relative.as_str().split('/').collect();
    let mut target = root.clone();
    for (index, segment) in segments.iter().enumerate() {
        let lower = segment.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or("");
        if matches!(
            lower.as_str(),
            ".git" | ".codex" | ".agents" | "node_modules" | "target" | "credentials" | "secrets"
        ) || lower.starts_with(".env")
            || matches!(stem, "con" | "prn" | "aux" | "nul")
            || (stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.as_bytes()[3].is_ascii_digit())
            || lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower.starts_with("id_rsa")
            || lower.starts_with("id_ed25519")
        {
            return Err(forbidden("Protected publication target."));
        }
        target.push(segment);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                reject_link(&target)?;
                if !target.canonicalize()?.starts_with(&root) {
                    return Err(forbidden("Destino fora do workspace."));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if create_parents && index + 1 < segments.len() {
                    fs::create_dir(&target)?;
                    reject_link(&target)?;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(target)
}

fn hash_file(path: &Path) -> Result<Option<ContentSha256>, Error> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_ARTIFACT_BYTES {
        return Err(forbidden(
            "The target is not a text file within the 1 MiB limit.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES || bytes.contains(&0) {
        return Err(forbidden("The target is binary or over the limit."));
    }
    Ok(Some(ContentSha256::of(&bytes)))
}

pub(crate) fn observed_hash(root: &Path, relative: &str) -> Result<Option<ContentSha256>, Error> {
    hash_file(&safe_target(root, relative, false)?)
}

pub(crate) fn publish(
    root: &Path,
    relative: &str,
    expected: &ExpectedFileContent,
    content: &str,
) -> Result<(), Error> {
    // The project review policy is written by the developer only (ADR-032).
    if super::composition::review_policy::is_review_policy_path(relative) {
        return Err(forbidden(
            "REVIEW.md is the project's review policy; no pipeline stage may publish it.",
        ));
    }
    if content.len() as u64 > MAX_ARTIFACT_BYTES || content.as_bytes().contains(&0) {
        return Err(forbidden("The artifact is binary or over the 1 MiB limit."));
    }
    let desired = ContentSha256::of(content.as_bytes());
    let target = safe_target(root, relative, false)?;
    match publication_replay_decision(expected, &desired, hash_file(&target)?.as_ref()) {
        PublicationReplayDecision::AcknowledgeAlreadyApplied => return Ok(()),
        PublicationReplayDecision::Conflict => {
            return Err(forbidden(
                "Publication conflict: the file changed outside this run; human content kept.",
            ));
        }
        PublicationReplayDecision::Apply => {}
    }
    let target = safe_target(root, relative, true)?;
    let parent = target
        .parent()
        .ok_or_else(|| forbidden("Invalid target."))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    temp.as_file().sync_all()?;
    // Recheck immediately before replacement. External editors do not share the host lock;
    // this is optimistic conflict detection, not an OS-wide filesystem transaction.
    safe_target(root, relative, false)?;
    match publication_replay_decision(expected, &desired, hash_file(&target)?.as_ref()) {
        PublicationReplayDecision::AcknowledgeAlreadyApplied => return Ok(()),
        PublicationReplayDecision::Conflict => {
            return Err(forbidden("Conflict detected before the atomic replace."));
        }
        PublicationReplayDecision::Apply => {}
    }
    if matches!(expected, ExpectedFileContent::Absent) {
        temp.persist_noclobber(&target)
            .map_err(|e| Error::Io(e.error))?;
    } else {
        temp.persist(&target).map_err(|e| Error::Io(e.error))?;
    }
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target)?
        .sync_all()?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
