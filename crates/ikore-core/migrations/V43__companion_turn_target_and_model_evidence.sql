-- Preserve the target of each assistant turn after a conversation is retargeted.
ALTER TABLE chat_messages ADD COLUMN target_label TEXT;

UPDATE chat_messages SET target_label = (
  SELECT mc.provider_id || ' / ' || mc.model_id ||
         CASE WHEN mc.model_profile IS NULL THEN '' ELSE ' / ' || mc.model_profile END
  FROM chat_model_calls mc
  WHERE mc.assistant_message_id = chat_messages.id
  ORDER BY mc.started_at, mc.id LIMIT 1
) WHERE role = 'assistant';

-- Requested and observed CLI model identities have different evidentiary value.
ALTER TABLE chat_model_calls ADD COLUMN observed_model TEXT;
ALTER TABLE chat_model_calls ADD COLUMN model_verification TEXT NOT NULL DEFAULT 'unverified'
  CHECK(model_verification IN ('unverified', 'verified', 'family_verified'));
