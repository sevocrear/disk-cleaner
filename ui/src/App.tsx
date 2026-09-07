import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  apiApplySelected,
  apiDeleteTrash,
  apiEmptyTrash,
  apiGetConfig,
  apiListDisks,
  apiListTrash,
  apiOpenPath,
  apiRestoreTrash,
  apiSaveConfig,
  apiStartScan,
  formatBytesLocal,
  parseSizeLocal,
} from "./api";
import type { Action, ApplySummary, Config, DiskInfo, Panel, PhaseResult, TrashItem } from "./types";

const NAV: { id: Panel; label: string }[] = [
  { id: "scan", label: "Deep clean" },
  { id: "review", label: "Review" },
  { id: "disks", label: "Disks" },
  { id: "trash", label: "Trash" },
  { id: "settings", label: "Settings" },
];

const PHASE_META: Record<string, { title: string; blurb: string; risk: "low" | "med" }> = {
  docker: { title: "Docker", blurb: "Stopped containers, unused images, build cache", risk: "med" },
  caches: { title: "Caches", blurb: "Old user cache entries and aged trash files", risk: "low" },
  files: { title: "Old files", blurb: "Large unused files in safe allowlisted roots", risk: "low" },
  apps: { title: "Unused apps", blurb: "Snap, Flatpak, and AppImage candidates", risk: "med" },
  dupes: { title: "Duplicates", blurb: "Identical content under Downloads and temp roots", risk: "low" },
  media: { title: "Media libraries", blurb: "Large unused files in Pictures, Videos, Music, Downloads", risk: "low" },
};

const SETTING_TIPS: Record<string, string> = {
  "Docker unused days":
    "Remove Docker images and stopped containers unused longer than this many days.",
  "File unused days":
    "Only consider files (and media libraries) whose last access and modification are both older than this.",
  "App unused days":
    "Flag Snap, Flatpak, and AppImage apps with no activity under this many days.",
  "Min file size":
    "Ignore files smaller than this when scanning old files and media. Accepts 1mb, 0.5kb, 5G; stored as bytes. Default 1 MiB.",
  "Min dupe size":
    "Only hash files at least this large when hunting duplicates. Accepts 1mb, 0.5kb, 5G. Larger = faster scans.",
  "Dedupe keep":
    "Which copy to keep in each duplicate group: newest, oldest, or first by path.",
  "Journal vacuum":
    "How far back to keep system journal logs when running as root (e.g. 7d, 2weeks).",
  "Confirm above":
    "CLI asks for confirmation when reclaimable space exceeds this. Accepts 1mb, 0.5kb, 5G. Default ~5 GiB.",
  "Move cleaned files to Trash (safer)":
    "GUI clean moves items to Trash instead of permanent delete. You can restore from the Trash panel.",
  "Include Docker volumes":
    "Also prune unused Docker volumes. Off by default — volumes can hold important data.",
  "Allow HuggingFace / torch cache cleanup":
    "Allow reclaiming HuggingFace / torch / transformers caches that are normally protected.",
  "Include package manager cache prune":
    "Also run uv/pip/npm cache prune. Can hang for a long time; off by default.",
};

export default function App() {
  const [panel, setPanel] = useState<Panel>("scan");
  const [config, setConfig] = useState<Config | null>(null);
  const [results, setResults] = useState<PhaseResult[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [scanPhase, setScanPhase] = useState("");
  const [disks, setDisks] = useState<DiskInfo[]>([]);
  const [trash, setTrash] = useState<TrashItem[]>([]);
  const [report, setReport] = useState<ApplySummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    apiGetConfig().then(setConfig).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ phase: string }>("scan-progress", (e) => {
      setScanPhase(e.payload.phase);
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      unlisten?.();
    };
  }, []);

  const refreshDisks = useCallback(() => {
    apiListDisks().then(setDisks).catch((e) => setError(String(e)));
  }, []);

  const refreshTrash = useCallback(() => {
    apiListTrash().then(setTrash).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (panel === "disks") refreshDisks();
    if (panel === "trash") refreshTrash();
  }, [panel, refreshDisks, refreshTrash]);

  const allActions = useMemo(
    () => results.flatMap((r) => r.actions.filter((a) => a.kind !== "rmdir_if_empty")),
    [results],
  );

  const selectedActions = useMemo(
    () => allActions.filter((a) => selected.has(a.id)),
    [allActions, selected],
  );

  const selectedBytes = selectedActions.reduce((s, a) => s + a.bytes, 0);

  async function runScan() {
    if (!config) return;
    setScanning(true);
    setError(null);
    setScanPhase("starting");
    try {
      const done = await apiStartScan(config);
      setResults(done.results);
      const ids = new Set(
        done.results.flatMap((r) =>
          r.actions.filter((a) => a.kind !== "rmdir_if_empty").map((a) => a.id),
        ),
      );
      setSelected(ids);
      setPanel("scan");
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
      setScanPhase("");
    }
  }

  async function cleanSelected() {
    if (!config || selectedActions.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const { summary } = await apiApplySelected(config, selectedActions);
      setReport(summary);
      const removed = new Set(selectedActions.map((a) => a.id));
      setResults((prev) =>
        prev.map((r) => ({
          ...r,
          actions: r.actions.filter((a) => !removed.has(a.id)),
          reclaimable_bytes: r.actions
            .filter((a) => !removed.has(a.id))
            .reduce((s, a) => s + a.bytes, 0),
        })),
      );
      setSelected(new Set());
      refreshDisks();
      refreshTrash();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  function togglePhase(name: string, skip: boolean) {
    if (!config) return;
    const key = `skip_${name}` as keyof Config;
    setConfig({ ...config, [key]: skip });
  }

  async function saveSettings(next?: Config) {
    const c = next ?? config;
    if (!c) return;
    setBusy(true);
    try {
      await apiSaveConfig(c);
      if (next) setConfig(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (!config) {
    return <div className="empty">Loading…</div>;
  }

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <img className="brand-logo" src="/logo.png" alt="" width={44} height={44} />
          <div>
            <h1>Disk Cleaner</h1>
            <p>Soft space, clear mind</p>
          </div>
        </div>
        {NAV.map((n) => (
          <button
            key={n.id}
            className={`nav-btn ${panel === n.id ? "active" : ""}`}
            onClick={() => setPanel(n.id)}
          >
            {n.label}
          </button>
        ))}
      </aside>

      <main className="main">
        {error && (
          <div style={{ padding: "12px 28px 0", color: "#7a4545" }}>{error}</div>
        )}

        {panel === "scan" && (
          <ScanPanel
            results={results}
            config={config}
            scanning={scanning}
            scanPhase={scanPhase}
            onScan={runScan}
            onTogglePhase={togglePhase}
            onOpenReview={() => setPanel("review")}
          />
        )}

        {panel === "review" && (
          <ReviewPanel
            actions={allActions}
            selected={selected}
            setSelected={setSelected}
          />
        )}

        {panel === "disks" && <DisksPanel disks={disks} onRefresh={refreshDisks} />}

        {panel === "trash" && (
          <TrashPanel
            items={trash}
            onRestore={async (item) => {
              await apiRestoreTrash(item);
              refreshTrash();
            }}
            onDelete={async (item) => {
              await apiDeleteTrash(item);
              refreshTrash();
            }}
            onEmpty={async () => {
              await apiEmptyTrash();
              refreshTrash();
            }}
          />
        )}

        {panel === "settings" && (
          <SettingsPanel config={config} setConfig={setConfig} onSave={saveSettings} busy={busy} />
        )}

        {(panel === "scan" || panel === "review") && (
          <div className="footer-bar">
            <div>
              <div className="meta">{selectedActions.length} selected</div>
              <div className="display-num" style={{ fontSize: "1.6rem" }}>
                {formatBytesLocal(selectedBytes)}
              </div>
            </div>
            <div style={{ display: "flex", gap: 10 }}>
              <button className="btn btn-ghost" onClick={() => setPanel("review")} disabled={!allActions.length}>
                Review
              </button>
              <button
                className="btn btn-primary"
                disabled={!selectedActions.length || busy || scanning}
                onClick={cleanSelected}
              >
                {busy ? "Cleaning…" : config.gui_use_trash ? "Move to Trash" : "Clean"}
              </button>
            </div>
          </div>
        )}
      </main>

      {report && (
        <div className="overlay" onClick={() => setReport(null)}>
          <div className="report-sheet" onClick={(e) => e.stopPropagation()}>
            <h3>Space returned</h3>
            <p style={{ color: "var(--muted)", margin: 0 }}>
              {report.files_deleted} items cleaned
              {report.failures ? ` · ${report.failures} failed` : ""}
            </p>
            <div className="big">{formatBytesLocal(report.bytes_reclaimed)}</div>
            <p style={{ color: "var(--muted)", marginTop: 0 }}>freed</p>
            <div style={{ textAlign: "left", margin: "18px 0 24px" }}>
              {report.by_phase.map(([phase, bytes]) => (
                <div
                  key={phase}
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    padding: "6px 0",
                    borderBottom: "1px solid var(--line)",
                  }}
                >
                  <span>{PHASE_META[phase]?.title || phase}</span>
                  <span>{formatBytesLocal(bytes)}</span>
                </div>
              ))}
            </div>
            <button className="btn btn-primary" onClick={() => setReport(null)}>
              Done
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function ScanPanel({
  results,
  config,
  scanning,
  scanPhase,
  onScan,
  onTogglePhase,
}: {
  results: PhaseResult[];
  config: Config;
  scanning: boolean;
  scanPhase: string;
  onScan: () => void;
  onTogglePhase: (name: string, skip: boolean) => void;
  onOpenReview: () => void;
}) {
  const byName = Object.fromEntries(results.map((r) => [r.name, r]));
  const total = results.reduce((s, r) => s + r.reclaimable_bytes, 0);
  const phases = Object.keys(PHASE_META);

  return (
    <div className="panel">
      <div className="panel-header" style={{ display: "flex", justifyContent: "space-between", gap: 16 }}>
        <div>
          <h2>Deep cleanup</h2>
          <p>Choose categories, scan safely, then review before anything is removed.</p>
        </div>
        <div style={{ textAlign: "right" }}>
          <div className="meta">Can reclaim</div>
          <div className="display-num">{formatBytesLocal(total)}</div>
        </div>
      </div>

      {scanning && (
        <div>
          <div className="progress">
            <span />
          </div>
          <p style={{ color: "var(--muted)", fontSize: "0.9rem" }}>Scanning {scanPhase || "…"}</p>
        </div>
      )}

      <div className="row-list">
        {phases.map((name) => {
          const meta = PHASE_META[name];
          const skipKey = `skip_${name}` as keyof Config;
          const skipped = Boolean(config[skipKey]);
          const r = byName[name];
          return (
            <div className="cat-row" key={name}>
              <input
                type="checkbox"
                checked={!skipped}
                onChange={(e) => onTogglePhase(name, !e.target.checked)}
              />
              <div>
                <strong>{meta.title}</strong>
                <div className="meta">{meta.blurb}</div>
              </div>
              <span className={`chip ${meta.risk}`}>{meta.risk === "low" ? "Low risk" : "Review"}</span>
              <strong>{r ? formatBytesLocal(r.reclaimable_bytes) : "—"}</strong>
            </div>
          );
        })}
      </div>

      <div>
        <button className="btn btn-primary" onClick={onScan} disabled={scanning}>
          {scanning ? "Scanning…" : results.length ? "Scan again" : "Scan"}
        </button>
      </div>
    </div>
  );
}

function ReviewPanel({
  actions,
  selected,
  setSelected,
}: {
  actions: Action[];
  selected: Set<string>;
  setSelected: (s: Set<string>) => void;
}) {
  if (!actions.length) {
    return (
      <div className="panel">
        <div className="panel-header">
          <h2>Review</h2>
          <p>Run a scan to see reclaimable items here.</p>
        </div>
        <div className="empty">Nothing to review yet</div>
      </div>
    );
  }

  return (
    <div className="panel">
      <div className="panel-header">
        <h2>Review</h2>
        <p>Check paths and sizes. Uncheck anything you want to keep.</p>
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <button
          className="btn btn-ghost"
          onClick={() => setSelected(new Set(actions.map((a) => a.id)))}
        >
          Select all
        </button>
        <button className="btn btn-ghost" onClick={() => setSelected(new Set())}>
          Clear
        </button>
      </div>
      <div className="row-list">
        {actions.map((a) => (
          <div className="file-row" key={a.id}>
            <input
              type="checkbox"
              checked={selected.has(a.id)}
              onChange={(e) => {
                const next = new Set(selected);
                if (e.target.checked) next.add(a.id);
                else next.delete(a.id);
                setSelected(next);
              }}
            />
            <div>
              <div className="path">{a.path}</div>
              <div className="meta">
                {a.phase} · {a.detail}
              </div>
            </div>
            <button className="btn btn-ghost" onClick={() => apiOpenPath(a.path)}>
              Open
            </button>
            <strong>{formatBytesLocal(a.bytes)}</strong>
          </div>
        ))}
      </div>
    </div>
  );
}

function DisksPanel({ disks, onRefresh }: { disks: DiskInfo[]; onRefresh: () => void }) {
  return (
    <div className="panel">
      <div className="panel-header" style={{ display: "flex", justifyContent: "space-between" }}>
        <div>
          <h2>Disks</h2>
          <p>Free space across mounted volumes.</p>
        </div>
        <button className="btn btn-ghost" onClick={onRefresh}>
          Refresh
        </button>
      </div>
      <div className="row-list">
        {disks.map((d) => {
          const pct = d.total_bytes ? (d.used_bytes / d.total_bytes) * 100 : 0;
          return (
            <div className="disk-row" key={d.mount_point}>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <strong>{d.mount_point}</strong>
                <span className="meta">{d.file_system}</span>
              </div>
              <div className="bar">
                <span style={{ width: `${pct}%` }} />
              </div>
              <div className="meta">
                {formatBytesLocal(d.available_bytes)} free of {formatBytesLocal(d.total_bytes)}
              </div>
            </div>
          );
        })}
        {!disks.length && <div className="empty">No disks found</div>}
      </div>
    </div>
  );
}

function TrashPanel({
  items,
  onRestore,
  onDelete,
  onEmpty,
}: {
  items: TrashItem[];
  onRestore: (i: TrashItem) => Promise<void>;
  onDelete: (i: TrashItem) => Promise<void>;
  onEmpty: () => Promise<void>;
}) {
  return (
    <div className="panel">
      <div className="panel-header" style={{ display: "flex", justifyContent: "space-between" }}>
        <div>
          <h2>Trash</h2>
          <p>Restore or permanently remove items from the XDG trash.</p>
        </div>
        <button className="btn btn-danger" onClick={() => onEmpty()} disabled={!items.length}>
          Empty trash
        </button>
      </div>
      <div className="row-list">
        {items.map((item) => (
          <div className="trash-row" key={item.trash_path}>
            <div>
              <strong>{item.name}</strong>
              <div className="path meta">{item.original_path || item.trash_path}</div>
              <div className="meta">
                {formatBytesLocal(item.bytes)}
                {item.deleted_at ? ` · ${item.deleted_at}` : ""}
              </div>
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <button className="btn btn-ghost" onClick={() => onRestore(item)}>
                Restore
              </button>
              <button className="btn btn-danger" onClick={() => onDelete(item)}>
                Delete
              </button>
            </div>
          </div>
        ))}
        {!items.length && <div className="empty">Trash is empty</div>}
      </div>
    </div>
  );
}

function SettingsPanel({
  config,
  setConfig,
  onSave,
  busy,
}: {
  config: Config;
  setConfig: (c: Config) => void;
  onSave: (next?: Config) => void;
  busy: boolean;
}) {
  const [minFileText, setMinFileText] = useState(() => formatBytesLocal(config.min_file_size));
  const [minDupeText, setMinDupeText] = useState(() => formatBytesLocal(config.min_dupe_size));
  const [confirmText, setConfirmText] = useState(() => formatBytesLocal(config.confirm_above));
  const [sizeError, setSizeError] = useState<string | null>(null);

  useEffect(() => {
    setMinFileText(formatBytesLocal(config.min_file_size));
  }, [config.min_file_size]);
  useEffect(() => {
    setMinDupeText(formatBytesLocal(config.min_dupe_size));
  }, [config.min_dupe_size]);
  useEffect(() => {
    setConfirmText(formatBytesLocal(config.confirm_above));
  }, [config.confirm_above]);

  function commitSizes(): Config | null {
    const minFile = parseSizeLocal(minFileText);
    const minDupe = parseSizeLocal(minDupeText);
    const confirm = parseSizeLocal(confirmText);
    if (minFile === null || minDupe === null || confirm === null) {
      setSizeError("Invalid size (try 1mb, 0.5kb, 5G)");
      return null;
    }
    setSizeError(null);
    const next = {
      ...config,
      min_file_size: minFile,
      min_dupe_size: minDupe,
      confirm_above: confirm,
    };
    setMinFileText(formatBytesLocal(minFile));
    setMinDupeText(formatBytesLocal(minDupe));
    setConfirmText(formatBytesLocal(confirm));
    setConfig(next);
    return next;
  }

  function handleSave() {
    const next = commitSizes();
    if (!next) return;
    onSave(next);
  }

  return (
    <div className="panel">
      <div className="panel-header">
        <h2>Settings</h2>
        <p>Thresholds and safety options. Hover a name for details. Saved to your user config.</p>
      </div>
      <div className="settings-grid">
        <Field
          label="Docker unused days"
          tip={SETTING_TIPS["Docker unused days"]}
          value={config.docker_unused_days}
          onChange={(v) => setConfig({ ...config, docker_unused_days: Number(v) })}
        />
        <Field
          label="File unused days"
          tip={SETTING_TIPS["File unused days"]}
          value={config.file_unused_days}
          onChange={(v) => setConfig({ ...config, file_unused_days: Number(v) })}
        />
        <Field
          label="App unused days"
          tip={SETTING_TIPS["App unused days"]}
          value={config.app_unused_days}
          onChange={(v) => setConfig({ ...config, app_unused_days: Number(v) })}
        />
        <SizeField
          label="Min file size"
          tip={SETTING_TIPS["Min file size"]}
          value={minFileText}
          onChange={(v) => {
            setMinFileText(v);
            setSizeError(null);
          }}
          onCommit={commitSizes}
        />
        <SizeField
          label="Min dupe size"
          tip={SETTING_TIPS["Min dupe size"]}
          value={minDupeText}
          onChange={(v) => {
            setMinDupeText(v);
            setSizeError(null);
          }}
          onCommit={commitSizes}
        />
        <div className="field">
          <TipLabel text="Dedupe keep" tip={SETTING_TIPS["Dedupe keep"]} />
          <select
            value={config.dedupe_keep}
            onChange={(e) => setConfig({ ...config, dedupe_keep: e.target.value })}
          >
            <option value="newest">newest</option>
            <option value="oldest">oldest</option>
            <option value="first">first</option>
          </select>
        </div>
        <Field
          label="Journal vacuum"
          tip={SETTING_TIPS["Journal vacuum"]}
          value={config.journal_vacuum}
          onChange={(v) => setConfig({ ...config, journal_vacuum: String(v) })}
        />
        <SizeField
          label="Confirm above"
          tip={SETTING_TIPS["Confirm above"]}
          value={confirmText}
          onChange={(v) => {
            setConfirmText(v);
            setSizeError(null);
          }}
          onCommit={commitSizes}
        />
      </div>
      {sizeError && (
        <p style={{ color: "#7a4545", margin: "0 0 12px" }}>{sizeError}</p>
      )}
      <div>
        <CheckOption
          label="Move cleaned files to Trash (safer)"
          tip={SETTING_TIPS["Move cleaned files to Trash (safer)"]}
          checked={config.gui_use_trash}
          onChange={(v) => setConfig({ ...config, gui_use_trash: v })}
        />
        <CheckOption
          label="Include Docker volumes"
          tip={SETTING_TIPS["Include Docker volumes"]}
          checked={config.include_docker_volumes}
          onChange={(v) => setConfig({ ...config, include_docker_volumes: v })}
        />
        <CheckOption
          label="Allow HuggingFace / torch cache cleanup"
          tip={SETTING_TIPS["Allow HuggingFace / torch cache cleanup"]}
          checked={config.include_hf_cache}
          onChange={(v) => setConfig({ ...config, include_hf_cache: v })}
        />
        <CheckOption
          label="Include package manager cache prune"
          tip={SETTING_TIPS["Include package manager cache prune"]}
          checked={config.include_pkg_managers}
          onChange={(v) => setConfig({ ...config, include_pkg_managers: v })}
        />
      </div>
      <div>
        <button className="btn btn-primary" onClick={handleSave} disabled={busy}>
          {busy ? "Saving…" : "Save settings"}
        </button>
      </div>
    </div>
  );
}

function TipLabel({ text, tip }: { text: string; tip: string }) {
  return (
    <label className="tip-label" data-tip={tip}>
      {text}
      <span className="tip-mark" aria-hidden>
        ?
      </span>
    </label>
  );
}

function Field({
  label,
  tip,
  value,
  onChange,
}: {
  label: string;
  tip: string;
  value: string | number;
  onChange: (v: string | number) => void;
}) {
  return (
    <div className="field">
      <TipLabel text={label} tip={tip} />
      <input value={value} onChange={(e) => onChange(e.target.value)} />
    </div>
  );
}

function SizeField({
  label,
  tip,
  value,
  onChange,
  onCommit,
}: {
  label: string;
  tip: string;
  value: string;
  onChange: (v: string) => void;
  onCommit: () => unknown;
}) {
  return (
    <div className="field">
      <TipLabel text={label} tip={tip} />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => {
          onCommit();
        }}
        placeholder="1mb"
        spellCheck={false}
      />
    </div>
  );
}

function CheckOption({
  label,
  tip,
  checked,
  onChange,
}: {
  label: string;
  tip: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="check tip-check" data-tip={tip}>
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span>{label}</span>
    </label>
  );
}
