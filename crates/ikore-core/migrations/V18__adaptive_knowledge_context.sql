INSERT OR IGNORE INTO runtime_settings(key,value,updated_at)
VALUES('runtime.adaptive_context_mode','shadow',CURRENT_TIMESTAMP);

INSERT OR IGNORE INTO runtime_settings(key,value,updated_at)
VALUES(
  'runtime.dynamic_context_policy_v1',
  '{"initialK":1,"maxK":3,"minSimilarity":0.65,"minCandidatesCount":3,"maxNotesBudget":8,"beamWidth":8,"maxSourceFiles":5,"maxCodeChars":32000,"timeoutMs":1500}',
  CURRENT_TIMESTAMP
);
