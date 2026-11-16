-- ADR-043: explicit capabilities (Qoder B1) and the host's MCP gateway
-- (Qoder B2, PI-Desktop PD12). The host is the MCP client; no CLI gets an
-- MCP server. Additive: four new tables, no rebuild of an existing table.
-- V66 is taken on another branch; the number is settled at integration.

-- Local MCP servers, global (no project) or of one project; a project
-- server hides a global one of its name. Variable values live in the
-- system's credential store: only their names are here.
CREATE TABLE mcp_servers (
  id TEXT PRIMARY KEY,
  project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
  command TEXT NOT NULL CHECK(length(command)>0),
  args_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(args_json)),
  env_names_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(env_names_json)),
  enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
  -- Nothing runs before the developer first connects the server.
  connected_at TEXT,
  last_error TEXT,
  version INTEGER NOT NULL DEFAULT 1 CHECK(version>=1),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_mcp_servers_name ON mcp_servers(COALESCE(project_id,''),name);

-- What a server listed when last connected, with the hash of each input
-- schema and the read-only hint the server declares.
CREATE TABLE mcp_tools (
  server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  input_schema_json TEXT NOT NULL CHECK(json_valid(input_schema_json)),
  schema_sha256 TEXT NOT NULL,
  read_only INTEGER NOT NULL CHECK(read_only IN (0,1)),
  listed_at TEXT NOT NULL,
  PRIMARY KEY(server_id,name)
);

-- Who may use what, in which project. An MCP grant covers the schema the
-- developer saw; a revoked grant stays for the record.
CREATE TABLE capability_grants (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  subject TEXT NOT NULL,
  capability TEXT NOT NULL,
  server_id TEXT REFERENCES mcp_servers(id) ON DELETE CASCADE,
  schema_sha256 TEXT,
  granted_at TEXT NOT NULL,
  revoked_at TEXT
);
CREATE UNIQUE INDEX idx_capability_grants_active
  ON capability_grants(project_id,subject,capability,server_id) WHERE revoked_at IS NULL;

-- Every use, refused or allowed, with the grant that allowed it.
CREATE TABLE capability_uses (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  subject TEXT NOT NULL,
  capability TEXT NOT NULL,
  grant_id TEXT REFERENCES capability_grants(id) ON DELETE SET NULL,
  outcome TEXT NOT NULL CHECK(outcome IN ('allowed','refused')),
  reason TEXT,
  status TEXT CHECK(status IS NULL OR status IN ('completed','failed')),
  arguments_json TEXT CHECK(arguments_json IS NULL OR json_valid(arguments_json)),
  result_bytes INTEGER CHECK(result_bytes IS NULL OR result_bytes>=0),
  duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms>=0),
  error TEXT,
  used_at TEXT NOT NULL
);
CREATE INDEX idx_capability_uses_project ON capability_uses(project_id,used_at);
