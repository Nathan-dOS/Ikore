-- ADR-033. Host-owned execution policy and evidence; the model wire V1 stays unchanged.
CREATE TABLE project_cargo_validation_policies (
    project_id TEXT NOT NULL,
    check_ref TEXT NOT NULL,
    profile_version INTEGER NOT NULL CHECK(profile_version >= 1),
    policy_json TEXT NOT NULL CHECK(json_valid(policy_json)),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(project_id, check_ref),
    FOREIGN KEY(project_id, check_ref)
      REFERENCES project_validation_profiles(project_id, check_ref) ON DELETE CASCADE
);

CREATE TABLE host_validation_run_details (
    receipt_id TEXT PRIMARY KEY REFERENCES host_validation_runs(id) ON DELETE CASCADE,
    details_json TEXT NOT NULL CHECK(json_valid(details_json))
);

-- Freeze the policy/tool bindings and captured input identity before any unit dispatch.
CREATE TABLE stage_cargo_validation_contexts (
    execution_id TEXT PRIMARY KEY REFERENCES stage_executions(id) ON DELETE CASCADE,
    baseline_sha256 TEXT NOT NULL,
    profiles_json TEXT NOT NULL CHECK(json_valid(profiles_json)),
    created_at TEXT NOT NULL
);
