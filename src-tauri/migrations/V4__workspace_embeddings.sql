CREATE TABLE IF NOT EXISTS workspace_note_embeddings (
  id TEXT PRIMARY KEY,
  workspace_path TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  revision_hash TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_vector_json TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(workspace_path, relative_path, embedding_model)
);

CREATE INDEX IF NOT EXISTS idx_workspace_note_embeddings_workspace_model
  ON workspace_note_embeddings(workspace_path, embedding_model);

CREATE INDEX IF NOT EXISTS idx_workspace_note_embeddings_workspace_path
  ON workspace_note_embeddings(workspace_path, relative_path);