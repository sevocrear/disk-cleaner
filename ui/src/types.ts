export type Action = {
  phase: string;
  kind: string;
  path: string;
  bytes: number;
  detail: string;
  command?: string[] | null;
  id: string;
};

export type PhaseResult = {
  name: string;
  reclaimable_bytes: number;
  actions: Action[];
  notes: string[];
};

export type Config = {
  apply: boolean;
  yes: boolean;
  docker_unused_days: number;
  file_unused_days: number;
  app_unused_days: number;
  min_file_size: number;
  min_dupe_size: number;
  dedupe_keep: string;
  extra_roots: string[];
  /** Mount points to include in Deep clean scans. Default: ["/"]. */
  scan_mounts: string[];
  dedupe_roots: string[];
  protect_globs: string[];
  protect_apps: string[];
  skip_docker: boolean;
  skip_caches: boolean;
  skip_files: boolean;
  skip_apps: boolean;
  skip_dupes: boolean;
  skip_media: boolean;
  only?: string | null;
  journal_vacuum: string;
  include_docker_volumes: boolean;
  include_hf_cache: boolean;
  include_opt_apps: boolean;
  confirm_above: number;
  cross_fs: boolean;
  home: string;
  report_dir?: string | null;
  file_roots?: string[] | null;
  cmd_timeout_sec: number;
  docker_timeout_sec: number;
  include_pkg_managers: boolean;
  gui_use_trash: boolean;
};

export type DiskInfo = {
  name: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
  used_bytes: number;
  file_system: string;
  is_removable: boolean;
};

export type TrashItem = {
  name: string;
  original_path: string;
  trash_path: string;
  info_path: string;
  bytes: number;
  deleted_at?: string | null;
  is_dir: boolean;
};

export type ApplySummary = {
  files_deleted: number;
  bytes_reclaimed: number;
  failures: number;
  by_phase: [string, number][];
};

export type Panel = "scan" | "review" | "disks" | "trash" | "settings";
