ALTER TABLE ui_sessions ADD COLUMN active_view TEXT NOT NULL DEFAULT 'workspace';
ALTER TABLE ui_sessions ADD COLUMN sidebar_collapsed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ui_sessions ADD COLUMN context_drawer_open INTEGER NOT NULL DEFAULT 1;
ALTER TABLE ui_sessions ADD COLUMN active_pulsation_id TEXT;
ALTER TABLE ui_sessions ADD COLUMN selected_vault_path TEXT;
