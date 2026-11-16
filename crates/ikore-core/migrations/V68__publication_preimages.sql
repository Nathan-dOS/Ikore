-- Reticle P1 with PI-Desktop PD6: the bytes each pipeline publication
-- replaced, kept after the pulsation ends. A run's changes compare them with
-- what it published; undoing a publication (Reticle P2) will restore them.
-- Unlike `pulsation_file_bases` (V51), which keeps only the first base of a
-- path for the Integrator and clears it when the pulsation ends, every
-- publication records what it replaced.
-- Additive: two new tables, no rebuild of an existing table.
-- V66 is taken on another branch; the number is settled at integration.

-- One copy of each replaced text per project, at most 1 MiB (the publication
-- limit). The host keeps the sum under 64 MiB per project (the V54 budget),
-- dropping texts no publication refers to before refusing a new one.
CREATE TABLE publication_preimage_blobs (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  content_sha256 TEXT NOT NULL,
  content TEXT NOT NULL,
  byte_len INTEGER NOT NULL CHECK(byte_len>=0 AND byte_len<=1048576),
  created_at TEXT NOT NULL,
  PRIMARY KEY(project_id,content_sha256)
);

-- What one applied publication replaced: nothing (absent), a stored text
-- (present) or a text it could not keep (present_unstored, with the reason).
CREATE TABLE publication_preimages (
  artifact_id TEXT PRIMARY KEY REFERENCES artifacts(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('absent','present','present_unstored')),
  content_sha256 TEXT,
  byte_len INTEGER CHECK(byte_len IS NULL OR byte_len>=0),
  reason TEXT CHECK(reason IS NULL OR reason IN ('too_large','replayed','unreadable','over_budget')),
  captured_at TEXT NOT NULL,
  CHECK((state='absent')=(content_sha256 IS NULL)),
  CHECK((state='present_unstored')=(reason IS NOT NULL))
);
CREATE INDEX idx_publication_preimages_blob ON publication_preimages(project_id,content_sha256);
CREATE INDEX idx_publication_preimages_pulsation ON publication_preimages(pulsation_id);
