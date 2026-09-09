-- Feature Agent driven Triage and Dependencies analysis.
--
-- `kind` lets the Incubation transcript and the discovery prompt filter to the
-- conversational turns only. Analysis turns still live in forge_turns so they
-- inherit the `forge_one_active_turn` mutual exclusion with chat, the restart
-- sweep and cancellation. The index deliberately stays keyed on
-- (feature_id) WHERE status='running' -- adding `kind` to it would let an
-- analysis and a chat turn run at the same time and race on the same feature.
--
-- `origin` marks who authored a dependency edge so an agent pass can be
-- reviewed or undone in bulk without touching human-authored relations.
-- Existing rows are backfilled by the defaults: prior turns are discovery and
-- prior edges are human, which is what they in fact are.
ALTER TABLE forge_turns ADD COLUMN kind TEXT NOT NULL DEFAULT 'discovery' CHECK(kind IN ('discovery','triage','dependencies'));
ALTER TABLE forge_feature_dependencies ADD COLUMN origin TEXT NOT NULL DEFAULT 'human' CHECK(origin IN ('human','agent'));
