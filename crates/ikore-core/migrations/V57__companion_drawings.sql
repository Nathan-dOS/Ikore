-- Native project drawings. SQLite owns the document; filesystem outputs are replayable.
CREATE TABLE companion_drawings (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 120),
    scene_json TEXT NOT NULL CHECK(json_valid(scene_json)),
    viewport_json TEXT NOT NULL CHECK(json_valid(viewport_json)),
    revision INTEGER NOT NULL CHECK(revision > 0),
    basename TEXT NOT NULL,
    mirrored_revision INTEGER NOT NULL DEFAULT 0 CHECK(mirrored_revision >= 0),
    json_sha256 TEXT,
    svg_sha256 TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, basename)
);
CREATE INDEX companion_drawings_project_updated
    ON companion_drawings(project_id, updated_at DESC, id);

-- No FK to a drawing: delete jobs must outlive the document they clean up.
-- Never replace an older pending job: its desired hashes describe partial writes.
CREATE TABLE companion_drawing_sync_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    drawing_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    operation TEXT NOT NULL CHECK(operation IN ('write', 'delete')),
    basename TEXT NOT NULL,
    json_content TEXT,
    svg_content TEXT,
    expected_json_sha256 TEXT,
    expected_svg_sha256 TEXT,
    desired_json_sha256 TEXT,
    desired_svg_sha256 TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'conflict', 'failed')),
    started INTEGER NOT NULL DEFAULT 0 CHECK(started IN (0,1)),
    error_message TEXT,
    CHECK((operation = 'write' AND json_content IS NOT NULL AND svg_content IS NOT NULL
           AND desired_json_sha256 IS NOT NULL AND desired_svg_sha256 IS NOT NULL)
          OR operation = 'delete')
);
CREATE INDEX companion_drawing_sync_project_order
    ON companion_drawing_sync_jobs(project_id, id);
CREATE INDEX companion_drawing_sync_drawing
    ON companion_drawing_sync_jobs(project_id, drawing_id, id);
