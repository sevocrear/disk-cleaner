import { startTransition, useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import type { Action, ApplyProgress, ApplySummary, Config, DiskInfo, Panel, PhaseResult, TrashItem } from "./types";

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
  files: {
    title: "Old files",
    blurb: "Large unused files in caches, temp, trash, and selected disks",
    risk: "low",
  },
  apps: { title: "Unused apps", blurb: "Snap, Flatpak, and AppImage candidates", risk: "med" },
  dupes: {
    title: "Duplicates",
    blurb: "Identical content under Downloads, temp, and selected disks",
    risk: "low",
  },
  media: {
    title: "Media libraries",
    blurb: "Large unused files only in Pictures, Videos, Music, Downloads",
    risk: "low",
  },
};

/** Above this, Review stays virtualized and we skip auto-selecting every item. */
const AUTO_SELECT_LIMIT = 2_000;
const REVIEW_ROW_HEIGHT = 72;
const REVIEW_OVERSCAN = 10;

const COMMAND_KINDS = new Set(["docker_cmd", "cache_cmd", "snap_remove", "flatpak_uninstall"]);

type PhaseStatus = "idle" | "pending" | "running" | "done";

type PhaseSummary = {
  reclaimable_bytes: number;
  action_count: number;
};

function isCommandAction(a: Action): boolean {
  return COMMAND_KINDS.has(a.kind) || Boolean(a.command?.length);
}

/** Default-selected: sized work, plus tiny network prune when listed. */
function isDefaultSelected(a: Action): boolean {
  return a.bytes > 0 || a.path === "network_prune";
}

function formatActionSize(a: Action): string {
  if (a.path === "network_prune" && a.bytes === 0) return "0B";
  return formatBytesLocal(a.bytes);
}

function formatPhaseSize(
  r: PhaseResult | undefined,
  summary: PhaseSummary | undefined,
  status: PhaseStatus,
): string {
  if (status === "pending" || status === "running") return "…";
  if (status === "idle") return "—";
  if (r) {
    const actions = r.actions.filter((a) => a.kind !== "rmdir_if_empty");
    if (!actions.length) return "—";
    const known = actions.reduce((s, a) => s + a.bytes, 0);
    if (known > 0) return formatBytesLocal(known);
    return "0B";
  }
  if (summary) {
    if (summary.action_count === 0) return "—";
    if (summary.reclaimable_bytes > 0) return formatBytesLocal(summary.reclaimable_bytes);
    return "0B";
  }
  return "—";
}

function formatSelectionSize(actions: Action[]): string {
  if (!actions.length) return formatBytesLocal(0);
  const known = actions.reduce((s, a) => s + a.bytes, 0);
  return formatBytesLocal(known);
}

function isOpenablePath(path: string): boolean {
  return path.startsWith("/") || path.startsWith("~");
}

function cleanButtonLabel(actions: Action[], useTrash: boolean, busy: boolean): string {
  if (busy) return "Cleaning…";
  if (!actions.length) return useTrash ? "Move to Trash" : "Clean";
  const onlyCommands = actions.every(isCommandAction);
  if (onlyCommands) return "Run commands";
  if (useTrash && actions.every((a) => !isCommandAction(a))) return "Move to Trash";
  return "Clean";
}

function emptyPhaseStatuses(): Record<string, PhaseStatus> {
  return Object.fromEntries(Object.keys(PHASE_META).map((k) => [k, "idle" as PhaseStatus]));
}

function phaseSkipped(config: Config, name: string): boolean {
  const key = `skip_${name}` as keyof Config;
  return Boolean(config[key]);
}

function defaultSelectedIds(actions: Action[]): Set<string> {
  const pick = actions.filter((a) => a.kind !== "rmdir_if_empty" && isDefaultSelected(a));
  if (pick.length > AUTO_SELECT_LIMIT) return new Set();
  return new Set(pick.map((a) => a.id));
}

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
    "Also remove unused Docker volumes (named and anonymous). Off by default — volumes can hold app data.",
  "Allow HuggingFace / torch cache cleanup":
    "Allow reclaiming HuggingFace / torch / transformers caches that are normally protected.",
};

export default function App() {
  const [panel, setPanel] = useState<Panel>("scan");
  const [config, setConfig] = useState<Config | null>(null);
  const [results, setResults] = useState<PhaseResult[]>([]);
  const [phaseSummaries, setPhaseSummaries] = useState<Record<string, PhaseSummary>>({});
  const [phaseStatus, setPhaseStatus] = useState<Record<string, PhaseStatus>>(emptyPhaseStatuses);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [scanPhase, setScanPhase] = useState("");
  const [scanDoneCount, setScanDoneCount] = useState(0);
  const [scanTotalCount, setScanTotalCount] = useState(0);
  const [disks, setDisks] = useState<DiskInfo[]>([]);
  const [trash, setTrash] = useState<TrashItem[]>([]);
  const [report, setReport] = useState<ApplySummary | null>(null);
  const [busy, setBusy] = useState(false);
  const [applyProgress, setApplyProgress] = useState<ApplyProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const applyProgressRef = useRef<ApplyProgress | null>(null);
  const applyRafRef = useRef<number | null>(null);

  useEffect(() => {
    apiGetConfig().then(setConfig).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    listen<{ phase: string }>("scan-phase-start", (e) => {
      const phase = e.payload.phase;
      setScanPhase(phase);
      setPhaseStatus((prev) => ({ ...prev, [phase]: "running" }));
    })
      .then((fn) => unsubs.push(fn))
      .catch(() => undefined);

    listen<{
      name: string;
      reclaimable_bytes: number;
      action_count: number;
      notes: string[];
    }>("scan-phase-done", (e) => {
      const { name, reclaimable_bytes, action_count } = e.payload;
      setPhaseSummaries((prev) => ({
        ...prev,
        [name]: { reclaimable_bytes, action_count },
      }));
      setPhaseStatus((prev) => ({ ...prev, [name]: "done" }));
      setScanDoneCount((n) => n + 1);
    })
      .then((fn) => unsubs.push(fn))
      .catch(() => undefined);

    listen<{ phase: string }>("scan-progress", (e) => {
      setScanPhase(e.payload.phase);
    })
      .then((fn) => unsubs.push(fn))
      .catch(() => undefined);

    // Coalesce apply-progress to one React update per animation frame.
    listen<ApplyProgress>("apply-progress", (e) => {
      applyProgressRef.current = e.payload;
      if (applyRafRef.current != null) return;
      applyRafRef.current = requestAnimationFrame(() => {
        applyRafRef.current = null;
        const latest = applyProgressRef.current;
        if (latest) startTransition(() => setApplyProgress(latest));
      });
    })
      .then((fn) => unsubs.push(fn))
      .catch(() => undefined);

    return () => {
      unsubs.forEach((u) => u());
      if (applyRafRef.current != null) cancelAnimationFrame(applyRafRef.current);
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

  const enabledPhases = useMemo(() => {
    if (!config) return new Set<string>();
    return new Set(Object.keys(PHASE_META).filter((name) => !phaseSkipped(config, name)));
  }, [config]);

  const visibleResults = useMemo(
    () => results.filter((r) => enabledPhases.has(r.name)),
    [results, enabledPhases],
  );

  const allActions = useMemo(
    () => visibleResults.flatMap((r) => r.actions.filter((a) => a.kind !== "rmdir_if_empty")),
    [visibleResults],
  );

  const selectedActions = useMemo(
    () => allActions.filter((a) => selected.has(a.id)),
    [allActions, selected],
  );

  const selectedSizeLabel = formatSelectionSize(selectedActions);

  const enabledPhaseCount = useMemo(() => enabledPhases.size, [enabledPhases]);

  // Drop selection for unchecked categories so footer/Review match the checkboxes.
  useEffect(() => {
    if (!config) return;
    const allowed = new Set(allActions.map((a) => a.id));
    setSelected((prev) => {
      let changed = false;
      const next = new Set<string>();
      for (const id of prev) {
        if (allowed.has(id)) next.add(id);
        else changed = true;
      }
      return changed ? next : prev;
    });
  }, [config, allActions]);

  async function runScan() {
    if (!config) return;
    setScanning(true);
    setError(null);
    setScanPhase("starting");
    setResults([]);
    setPhaseSummaries({});
    setSelected(new Set());
    setScanDoneCount(0);
    setScanTotalCount(enabledPhaseCount);
    const pending: Record<string, PhaseStatus> = emptyPhaseStatuses();
    for (const name of Object.keys(PHASE_META)) {
      pending[name] = phaseSkipped(config, name) ? "idle" : "pending";
    }
    setPhaseStatus(pending);
    try {
      const done = await apiStartScan(config);
      startTransition(() => {
        setResults(done.results);
        const enabled = done.results.filter((r) => !phaseSkipped(config, r.name));
        setSelected(defaultSelectedIds(enabled.flatMap((r) => r.actions)));
        setPhaseStatus((prev) => {
          const next = { ...prev };
          for (const r of done.results) next[r.name] = "done";
          return next;
        });
        setPhaseSummaries((prev) => {
          const next = { ...prev };
          for (const r of done.results) {
            next[r.name] = {
              reclaimable_bytes: r.reclaimable_bytes,
              action_count: r.actions.filter((a) => a.kind !== "rmdir_if_empty").length,
            };
          }
          return next;
        });
      });
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
    setApplyProgress({
      done: 0,
      total: selectedActions.length,
      bytes_reclaimed: 0,
      failures: 0,
      path: "",
    });
    try {
      const { summary } = await apiApplySelected(config, selectedActions);
      setReport(summary);
      const removed = new Set(selectedActions.map((a) => a.id));
      startTransition(() => {
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
      });
      refreshDisks();
      refreshTrash();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setApplyProgress(null);
      applyProgressRef.current = null;
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
        {scanning && (
          <div className="scan-banner" aria-live="polite">
            Scanning {scanDoneCount}/{scanTotalCount || "…"}
            {scanPhase ? ` · ${scanPhase}` : ""}
          </div>
        )}
      </aside>

      <main className="main">
        {error && (
          <div style={{ padding: "12px 28px 0", color: "#7a4545" }}>{error}</div>
        )}

        {panel === "scan" && (
          <ScanPanel
            results={results}
            phaseSummaries={phaseSummaries}
            phaseStatus={phaseStatus}
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
            autoSelectSkipped={allActions.filter(isDefaultSelected).length > AUTO_SELECT_LIMIT}
          />
        )}

        {panel === "disks" && (
          <DisksPanel
            disks={disks}
            scanMounts={config.scan_mounts ?? ["/"]}
            onRefresh={refreshDisks}
            onToggleMount={(mount, on) => {
              const cur = [...(config.scan_mounts ?? ["/"])];
              const next = on
                ? cur.includes(mount)
                  ? cur
                  : [...cur, mount]
                : cur.filter((m) => m !== mount);
              const nextCfg = { ...config, scan_mounts: next };
              setConfig(nextCfg);
              apiSaveConfig(nextCfg).catch((e) => setError(String(e)));
            }}
          />
        )}

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
            <div style={{ minWidth: 0, flex: 1 }}>
              <div className="meta">
                {scanning
                  ? `Scanning ${scanDoneCount}/${scanTotalCount || "…"}`
                  : busy && applyProgress
                    ? `Cleaning ${applyProgress.done.toLocaleString()}/${applyProgress.total.toLocaleString()}${
                        applyProgress.failures ? ` · ${applyProgress.failures} failed` : ""
                      }`
                    : `${selectedActions.length} selected`}
              </div>
              <div className="display-num" style={{ fontSize: "1.6rem" }}>
                {scanning
                  ? "…"
                  : busy && applyProgress
                    ? formatBytesLocal(applyProgress.bytes_reclaimed)
                    : selectedSizeLabel}
              </div>
              {busy && applyProgress && applyProgress.total > 0 && (
                <div className="progress progress-determinate" style={{ marginTop: 8, maxWidth: 280 }}>
                  <span
                    style={{
                      width: `${Math.min(100, (100 * applyProgress.done) / applyProgress.total)}%`,
                    }}
                  />
                </div>
              )}
            </div>
            <div style={{ display: "flex", gap: 10 }}>
              <button className="btn btn-ghost" onClick={() => setPanel("review")} disabled={!allActions.length && !scanning}>
                Review
              </button>
              <button
                className="btn btn-primary"
                disabled={!selectedActions.length || busy || scanning}
                onClick={cleanSelected}
              >
                {scanning
                  ? "Scanning…"
                  : cleanButtonLabel(selectedActions, config.gui_use_trash, busy)}
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
  phaseSummaries,
  phaseStatus,
  config,
  scanning,
  scanPhase,
  onScan,
  onTogglePhase,
  onOpenReview,
}: {
  results: PhaseResult[];
  phaseSummaries: Record<string, PhaseSummary>;
  phaseStatus: Record<string, PhaseStatus>;
  config: Config;
  scanning: boolean;
  scanPhase: string;
  onScan: () => void;
  onTogglePhase: (name: string, skip: boolean) => void;
  onOpenReview: () => void;
}) {
  const byName = Object.fromEntries(results.map((r) => [r.name, r]));
  const phases = Object.keys(PHASE_META);
  const anyDone = phases.some((n) => phaseStatus[n] === "done");
  const enabled = phases.filter((n) => !phaseSkipped(config, n));
  const knownTotal = enabled.reduce((s, name) => {
    const r = byName[name];
    if (r) {
      return (
        s + r.actions.filter((a) => a.kind !== "rmdir_if_empty").reduce((x, a) => x + a.bytes, 0)
      );
    }
    return s + (phaseSummaries[name]?.reclaimable_bytes || 0);
  }, 0);
  const totalLabel = scanning
    ? "…"
    : anyDone || results.length
      ? knownTotal > 0
        ? formatBytesLocal(knownTotal)
        : enabled.some((name) => {
            const r = byName[name];
            if (r) return r.actions.some((a) => a.kind !== "rmdir_if_empty");
            return (phaseSummaries[name]?.action_count || 0) > 0;
          })
          ? "0B"
          : "—"
      : "—";

  return (
    <div className="panel">
      <div className="panel-header" style={{ display: "flex", justifyContent: "space-between", gap: 16 }}>
        <div>
          <h2>Deep cleanup</h2>
          <p>Choose categories, scan safely, then review before anything is removed.</p>
        </div>
        <div style={{ textAlign: "right" }}>
          <div className="meta">Can reclaim</div>
          <div className="display-num">{totalLabel}</div>
        </div>
      </div>

      {scanning && (
        <div>
          <div className="progress">
            <span />
          </div>
          <p style={{ color: "var(--muted)", fontSize: "0.9rem" }}>
            Scanning {scanPhase || "…"} (categories run in parallel)
          </p>
        </div>
      )}

      <div className="row-list">
        {phases.map((name) => {
          const meta = PHASE_META[name];
          const skipped = phaseSkipped(config, name);
          const r = byName[name];
          const status = skipped ? "idle" : phaseStatus[name] || "idle";
          return (
            <div className="cat-row" key={name}>
              <input
                type="checkbox"
                checked={!skipped}
                disabled={scanning}
                onChange={(e) => onTogglePhase(name, !e.target.checked)}
              />
              <div>
                <strong>{meta.title}</strong>
                <div className="meta">{meta.blurb}</div>
              </div>
              <span className={`chip ${meta.risk}`}>{meta.risk === "low" ? "Low risk" : "Review"}</span>
              <strong>{skipped ? "—" : formatPhaseSize(r, phaseSummaries[name], status)}</strong>
            </div>
          );
        })}
      </div>

      <div>
        <button className="btn btn-primary" onClick={onScan} disabled={scanning}>
          {scanning ? "Scanning…" : results.length || anyDone ? "Scan again" : "Scan"}
        </button>
        {(results.length > 0 || anyDone) && !scanning && (
          <button className="btn btn-ghost" style={{ marginLeft: 8 }} onClick={onOpenReview}>
            Review
          </button>
        )}
      </div>
    </div>
  );
}

function ReviewPanel({
  actions,
  selected,
  setSelected,
  autoSelectSkipped,
}: {
  actions: Action[];
  selected: Set<string>;
  setSelected: (s: Set<string>) => void;
  autoSelectSkipped: boolean;
}) {
  if (!actions.length) {
    return (
      <div className="panel">
        <div className="panel-header">
          <h2>Review</h2>
          <p>Only checked categories from Deep clean appear here.</p>
        </div>
        <div className="empty">Nothing to review yet — enable a category and scan.</div>
      </div>
    );
  }

  function selectSized() {
    startTransition(() => {
      setSelected(new Set(actions.filter(isDefaultSelected).map((a) => a.id)));
    });
  }

  function selectAll() {
    startTransition(() => {
      setSelected(new Set(actions.map((a) => a.id)));
    });
  }

  return (
    <div className="panel review-panel">
      <div className="panel-header">
        <h2>Review</h2>
        <p>
          Showing {actions.length.toLocaleString()} items from checked categories only.
          {autoSelectSkipped && selected.size === 0
            ? " Large result — nothing pre-selected; use Select sized / Select all."
            : ""}
        </p>
      </div>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button className="btn btn-ghost" onClick={selectSized}>
          Select sized
        </button>
        <button className="btn btn-ghost" onClick={selectAll}>
          Select all
        </button>
        <button className="btn btn-ghost" onClick={() => setSelected(new Set())}>
          Clear
        </button>
      </div>
      <VirtualActionList actions={actions} selected={selected} setSelected={setSelected} />
    </div>
  );
}

function VirtualActionList({
  actions,
  selected,
  setSelected,
}: {
  actions: Action[];
  selected: Set<string>;
  setSelected: (s: Set<string>) => void;
}) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(480);

  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    const sync = () => setViewportH(el.clientHeight || 480);
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const totalH = actions.length * REVIEW_ROW_HEIGHT;
  const start = Math.max(0, Math.floor(scrollTop / REVIEW_ROW_HEIGHT) - REVIEW_OVERSCAN);
  const visibleCount = Math.ceil(viewportH / REVIEW_ROW_HEIGHT) + REVIEW_OVERSCAN * 2;
  const end = Math.min(actions.length, start + visibleCount);
  const slice = actions.slice(start, end);

  return (
    <div
      className="virtual-list"
      ref={scrollerRef}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      <div className="virtual-list-spacer" style={{ height: totalH }}>
        <div
          className="virtual-list-window"
          style={{ transform: `translateY(${start * REVIEW_ROW_HEIGHT}px)` }}
        >
          {slice.map((a) => (
            <div className="file-row" key={a.id} style={{ height: REVIEW_ROW_HEIGHT }}>
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
              <div className="file-row-text">
                <div className="path" title={a.path}>
                  {a.path}
                </div>
                <div className="meta">
                  {a.phase} · {a.detail}
                  {isCommandAction(a) ? " · command" : ""}
                </div>
              </div>
              {isOpenablePath(a.path) ? (
                <button className="btn btn-ghost" onClick={() => apiOpenPath(a.path)}>
                  Open
                </button>
              ) : (
                <span className="meta" style={{ minWidth: 52, textAlign: "center" }}>
                  —
                </span>
              )}
              <strong>{formatActionSize(a)}</strong>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function DisksPanel({
  disks,
  scanMounts,
  onRefresh,
  onToggleMount,
}: {
  disks: DiskInfo[];
  scanMounts: string[];
  onRefresh: () => void;
  onToggleMount: (mount: string, on: boolean) => void;
}) {
  const selected = new Set(scanMounts);
  return (
    <div className="panel">
      <div className="panel-header" style={{ display: "flex", justifyContent: "space-between" }}>
        <div>
          <h2>Disks</h2>
          <p>Choose which mounted volumes Deep clean should scan. `/` is on by default.</p>
        </div>
        <button className="btn btn-ghost" onClick={onRefresh}>
          Refresh
        </button>
      </div>
      <div className="row-list">
        {disks.map((d) => {
          const pct = d.total_bytes ? (d.used_bytes / d.total_bytes) * 100 : 0;
          const checked = selected.has(d.mount_point);
          return (
            <label className="disk-row" key={d.mount_point}>
              <input
                type="checkbox"
                checked={checked}
                onChange={(e) => onToggleMount(d.mount_point, e.target.checked)}
              />
              <div className="disk-row-body">
                <div style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
                  <div>
                    <strong>{d.mount_point}</strong>
                    {d.name && d.name !== d.mount_point && (
                      <div className="meta disk-device">{d.name}</div>
                    )}
                  </div>
                  <span className="meta">{d.file_system}</span>
                </div>
                <div className="bar">
                  <span style={{ width: `${pct}%` }} />
                </div>
                <div className="meta">
                  {formatBytesLocal(d.available_bytes)} free of {formatBytesLocal(d.total_bytes)}
                  {checked ? " · included in scan" : ""}
                </div>
              </div>
            </label>
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
