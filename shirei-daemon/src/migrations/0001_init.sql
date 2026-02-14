CREATE TABLE IF NOT EXISTS projects (
  project_id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL UNIQUE,
  git_remote TEXT NOT NULL,
  default_branch TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS jobs (
  job_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  name TEXT NOT NULL,
  status TEXT NOT NULL,
  needs_attention INTEGER NOT NULL DEFAULT 0,

  runner TEXT NOT NULL,
  cmd TEXT NOT NULL,
  argv_json TEXT NOT NULL,
  cwd TEXT NOT NULL,

  pid INTEGER,
  log_path TEXT,
  prompt TEXT,

  acknowledged_at TEXT,

  origin_type TEXT NOT NULL DEFAULT 'manual',
  origin_repo_key TEXT,
  origin_pr_number INTEGER,
  origin_command_id TEXT,

  worktree_name TEXT,
  worktree_path TEXT,

  started_at TEXT,
  ended_at TEXT,
  exit_code INTEGER,
  closed_at TEXT,

  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,

  FOREIGN KEY(project_id) REFERENCES projects(project_id)
);

CREATE INDEX IF NOT EXISTS idx_jobs_project_status ON jobs(project_id, status);
CREATE INDEX IF NOT EXISTS idx_jobs_origin ON jobs(origin_type, origin_repo_key, origin_pr_number);

CREATE TABLE IF NOT EXISTS job_events (
  event_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  job_id TEXT NOT NULL,
  type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  ts TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_job_events_job_ts ON job_events(job_id, ts DESC);

CREATE TABLE IF NOT EXISTS pr_watches (
  watch_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  repo_key TEXT NOT NULL, -- github:owner/repo OR gitlab:group/project
  pr_number INTEGER NOT NULL,
  retention_days INTEGER NOT NULL DEFAULT 7,

  status TEXT NOT NULL DEFAULT 'RUNNING', -- RUNNING|STOPPED
  last_seen_comment_id INTEGER NOT NULL DEFAULT 0,

  merged_at TEXT,
  purge_due_at TEXT,
  purged_at TEXT,

  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,

  UNIQUE(repo_key, pr_number),
  FOREIGN KEY(project_id) REFERENCES projects(project_id)
);

CREATE TABLE IF NOT EXISTS pr_commands (
  command_id TEXT PRIMARY KEY,
  watch_id TEXT NOT NULL,
  source_id INTEGER NOT NULL,
  author_login TEXT NOT NULL,
  raw_text TEXT NOT NULL,
  status TEXT NOT NULL, -- RECEIVED|RUNNING|DONE|FAILED|IGNORED
  job_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(watch_id, source_id),
  FOREIGN KEY(watch_id) REFERENCES pr_watches(watch_id)
);
