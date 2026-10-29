ALTER TABLE source_scan ADD COLUMN group_memberships_state TEXT NOT NULL DEFAULT 'complete'
  CHECK (group_memberships_state IN ('complete','partial','unavailable'));
