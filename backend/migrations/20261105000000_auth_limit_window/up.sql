-- Every rate-limited request deletes expired windows first; without this index
-- that DELETE scans every key (each connected socket and chat sender has some).
CREATE INDEX auth_limits_window_start ON auth_limits (window_start);
