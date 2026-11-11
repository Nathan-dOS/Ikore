-- Explicit removal of files a failed pulsation created in the workspace.
-- `removed_at` marks an applied main publication whose file the operator
-- removed through the cleanup; the row itself stays as the durable record
-- of what was written and when it was undone.
ALTER TABLE artifact_publications ADD COLUMN removed_at TEXT;
