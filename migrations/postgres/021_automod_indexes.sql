-- Hot paths that previously scanned: cache expiry pruning, per-space event
-- listing, and the deletion drain's liveness check by URL.
CREATE INDEX automod_cache_expiry ON automod_cache(expires_at);
CREATE INDEX automod_events_scope ON automod_events(scope_id, id);
CREATE INDEX attachments_url ON attachments(url);
