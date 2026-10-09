import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  apiListDisks,
  apiOpenPath,
  apiOverviewCancel,
  apiOverviewDelete,
  apiOverviewInUse,
  apiOverviewList,
  apiOverviewScan,
  formatBytesLocal,
} from "./api";
import { ConfirmDialog } from "./ConfirmDialog";
import {
  breadcrumbs,
  canSelect,
  entryBadges,
  formatAge,
  pickedBytes,
  prunePicks,
  share,
  togglePick,
  topLevelPicks,
  type PickMap,
} from "./overview";
import type {
  Config,
  DeleteOutcome,
  DirListing,
  DiskInfo,
  InUse,
  SpaceAccounting,
  TreeEntry,
  TreeScanProgress,
} from "./types";

const PAGE = 300;

type Pending = { useTrash: boolean; inUse: InUse[] | null };

export function OverviewPanel({ config, onChanged }: { config: Config; onChanged: () => void }) {
  const [rootInput, setRootInput] = useState(config.home || "/");
  const [crossFs, setCrossFs] = useState(false);
  const [disks, setDisks] = useState<DiskInfo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [progress, setProgress] = useState<TreeScanProgress | null>(null);
  const [root, setRoot] = useState<string | null>(null);
  const [accounting, setAccounting] = useState<SpaceAccounting | null>(null);
  const [listing, setListing] = useState<DirListing | null>(null);
  const [cursor, setCursor] = useState(0);
  const [limit, setLimit] = useState(PAGE);
  const [picks, setPicks] = useState<PickMap>(() => new Map());
  const [pending, setPending] = useState<Pending | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [result, setResult] = useState<DeleteOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const cursorMemo = useRef(new Map<string, number>());
  const listRef = useRef<HTMLDivElement>(null);
  const now = Math.floor(Date.now() / 1000);

  useEffect(() => {
    apiListDisks().then(setDisks).catch(() => undefined);
    let unsub: (() => void) | undefined;
    listen<TreeScanProgress>("overview-progress", (e) => setProgress(e.payload))
      .then((fn) => (unsub = fn))
      .catch(() => undefined);
    return () => unsub?.();
  }, []);

  const entries = listing?.entries ?? [];
  const visible = entries.slice(0, limit);

  const goTo = useCallback(
    async (path: string, focusChild?: string) => {
      try {
        if (listing) cursorMemo.current.set(listing.path, cursor);
        const next = await apiOverviewList(path);
        setListing(next);
        setLimit(PAGE);
        const back = focusChild ? next.entries.findIndex((e) => e.path === focusChild) : -1;
        setCursor(back >= 0 ? back : cursorMemo.current.get(path) ?? 0);
        setError(null);
        listRef.current?.focus();
      } catch (e) {
        setError(String(e));
      }
    },
    [listing, cursor],
  );

  async function scan(path = rootInput) {
    const target = path.trim();
    if (!target) return;
    setRootInput(target);
    setScanning(true);
    setError(null);
    setResult(null);
    setProgress(null);
    setPicks(new Map());
    cursorMemo.current.clear();
    try {
      const done = await apiOverviewScan(target, crossFs);
      setRoot(done.root);
      setAccounting(done.accounting ?? null);
      setListing(done.listing ?? null);
      setCursor(0);
      setLimit(PAGE);
      if (done.cancelled) setError("Scan cancelled — sizes are partial.");
      listRef.current?.focus();
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  }

  async function askDelete(useTrash: boolean) {
    const targets = topLevelPicks(picks).map(([p]) => p);
    if (!targets.length) return;
    setPending({ useTrash, inUse: null });
    try {
      const inUse = await apiOverviewInUse(targets);
      setPending((p) => (p ? { ...p, inUse } : p));
    } catch {
      setPending((p) => (p ? { ...p, inUse: [] } : p));
    }
  }

  async function confirmDelete() {
    if (!pending || !listing) return;
    setDeleting(true);
    try {
      const targets = topLevelPicks(picks).map(([p]) => p);
      const { outcome } = await apiOverviewDelete(targets, pending.useTrash);
      setResult(outcome);
      setPicks((prev) => prunePicks(prev, outcome.removed));
      // The current dir may itself have been deleted; climb to the nearest survivor.
      let path = listing.path;
      while (outcome.removed.some((r) => path === r || path.startsWith(`${r}/`)) && path !== root) {
        path = path.slice(0, path.lastIndexOf("/")) || "/";
      }
      const next = await apiOverviewList(path);
      setListing(next);
      setCursor((c) => Math.min(c, Math.max(0, next.entries.length - 1)));
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
      setPending(null);
    }
  }

  function toggle(e: TreeEntry) {
    if (!canSelect(e)) return;
    setPicks((prev) => togglePick(prev, e));
  }

  function open(e: TreeEntry) {
    if (e.kind === "dir") void goTo(e.path);
  }

  function goUp() {
    if (listing?.parent) void goTo(listing.parent, listing.path);
  }

  function onListKey(ev: KeyboardEvent<HTMLDivElement>) {
    if (!listing || pending) return;
    const cur = visible[cursor];
    switch (ev.key) {
      case "ArrowDown":
      case "j":
        setCursor((c) => Math.min(visible.length - 1, c + 1));
        break;
      case "ArrowUp":
      case "k":
        setCursor((c) => Math.max(0, c - 1));
        break;
      case "Home":
        setCursor(0);
        break;
      case "End":
        setCursor(Math.max(0, visible.length - 1));
        break;
      case "Enter":
      case "ArrowRight":
        if (cur) open(cur);
        break;
      case "Backspace":
      case "ArrowLeft":
        goUp();
        break;
      case " ":
        if (cur) toggle(cur);
        setCursor((c) => Math.min(visible.length - 1, c + 1));
        break;
      case "Delete":
        if (picks.size) void askDelete(config.gui_use_trash);
        else if (cur && canSelect(cur)) {
          setPicks(new Map([[cur.path, cur.bytes]]));
          setPending({ useTrash: config.gui_use_trash, inUse: null });
          apiOverviewInUse([cur.path])
            .then((inUse) => setPending((p) => (p ? { ...p, inUse } : p)))
            .catch(() => setPending((p) => (p ? { ...p, inUse: [] } : p)));
        }
        break;
      default:
        return;
    }
    ev.preventDefault();
  }

  // Keep the focused row in view while moving with the keyboard.
  useEffect(() => {
    const row = listRef.current?.querySelector<HTMLElement>(`[data-index="${cursor}"]`);
    row?.scrollIntoView?.({ block: "nearest" });
  }, [cursor]);

  const crumbs = useMemo(() => (root && listing ? breadcrumbs(root, listing.path) : []), [root, listing]);
  const pickTotal = pickedBytes(picks);
  const roots = useMemo(() => {
    const out = [{ label: "Home", path: config.home }];
    for (const d of disks) out.push({ label: d.mount_point, path: d.mount_point });
    return out.filter((r, i, a) => r.path && a.findIndex((x) => x.path === r.path) === i);
  }, [disks, config.home]);

  return (
    <div className="panel overview-panel">
      <div className="panel-header">
        <h2>Overview</h2>
        <p>See where space goes, drill into folders, and delete what you no longer need.</p>
      </div>

      <form
        className="overview-roots"
        onSubmit={(e) => {
          e.preventDefault();
          void scan();
        }}
      >
        <input
          aria-label="Folder to scan"
          className="root-input"
          value={rootInput}
          onChange={(e) => setRootInput(e.target.value)}
          spellCheck={false}
          disabled={scanning}
        />
        {scanning ? (
          <button type="button" className="btn btn-ghost" onClick={() => apiOverviewCancel()}>
            Cancel
          </button>
        ) : (
          <button type="submit" className="btn btn-primary">
            {root ? "Rescan" : "Scan"}
          </button>
        )}
        <div className="root-chips">
          {roots.map((r) => (
            <button
              type="button"
              key={r.path}
              className="chip chip-btn"
              disabled={scanning}
              onClick={() => void scan(r.path)}
            >
              {r.label}
            </button>
          ))}
          <label className="check small">
            <input type="checkbox" checked={crossFs} onChange={(e) => setCrossFs(e.target.checked)} />
            Cross filesystems
          </label>
        </div>
      </form>

      {error && <div className="inline-error" role="alert">{error}</div>}

      {scanning && (
        <div className="scan-status" aria-live="polite">
          <div className="progress">
            <span />
          </div>
          <p className="meta">
            {progress
              ? `${progress.files.toLocaleString()} files · ${progress.dirs.toLocaleString()} folders · ${formatBytesLocal(progress.bytes)}`
              : "Scanning…"}
          </p>
          {progress?.current && <p className="meta path">{progress.current}</p>}
        </div>
      )}

      {!scanning && accounting && <SpaceCard acc={accounting} />}

      {!scanning && listing && (
        <>
          <nav className="crumbs" aria-label="Breadcrumbs">
            <button className="btn btn-ghost btn-sm" onClick={goUp} disabled={!listing.parent} aria-label="Up one level">
              ↑ Up
            </button>
            {crumbs.map((c, i) => (
              <span key={c.path} className="crumb">
                {i > 0 && <span className="crumb-sep">/</span>}
                <button
                  className="crumb-btn"
                  onClick={() => void goTo(c.path)}
                  disabled={i === crumbs.length - 1}
                  aria-current={i === crumbs.length - 1 ? "location" : undefined}
                >
                  {c.label}
                </button>
              </span>
            ))}
            <span className="meta crumb-total">
              {formatBytesLocal(listing.bytes)} · {listing.items.toLocaleString()} items
            </span>
          </nav>

          <div
            className="tree-list"
            role="grid"
            aria-label="Folder contents"
            tabIndex={0}
            ref={listRef}
            onKeyDown={onListKey}
          >
            {visible.map((e, i) => (
              <TreeRow
                key={e.path || e.name}
                entry={e}
                index={i}
                total={listing.bytes}
                now={now}
                focused={i === cursor}
                picked={picks.has(e.path)}
                onFocus={() => setCursor(i)}
                onToggle={() => toggle(e)}
                onOpen={() => open(e)}
              />
            ))}
            {!entries.length && (
              <div className="empty">{listing.unreadable ? "Cannot read this folder." : "This folder is empty."}</div>
            )}
            {entries.length > limit && (
              <button className="btn btn-ghost show-more" onClick={() => setLimit((l) => l + PAGE)}>
                Show {Math.min(PAGE, entries.length - limit)} more of {entries.length - limit}
              </button>
            )}
          </div>
          <p className="meta keys-hint">
            Keys: ↑↓ move · Enter open · Backspace up · Space select · Delete remove
          </p>
        </>
      )}

      {!scanning && !listing && !error && (
        <div className="empty">Pick a folder or disk and scan to see what takes space.</div>
      )}

      <div className="overview-footer">
        <div>
          <div className="meta">{picks.size} selected</div>
          <div className="display-num" style={{ fontSize: "1.5rem" }}>
            {formatBytesLocal(pickTotal)}
          </div>
        </div>
        <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
          <button className="btn btn-ghost" disabled={!picks.size} onClick={() => setPicks(new Map())}>
            Clear
          </button>
          <button className="btn btn-primary" disabled={!picks.size || deleting} onClick={() => void askDelete(true)}>
            Move to Trash
          </button>
          <button className="btn btn-danger" disabled={!picks.size || deleting} onClick={() => void askDelete(false)}>
            Delete permanently
          </button>
        </div>
      </div>

      {pending && (
        <ConfirmDialog
          title={pending.useTrash ? "Move to Trash?" : "Delete permanently?"}
          confirmLabel={pending.useTrash ? "Move to Trash" : "Delete permanently"}
          danger={!pending.useTrash}
          busy={deleting}
          onCancel={() => setPending(null)}
          onConfirm={() => void confirmDelete()}
        >
          <ConfirmBody picks={picks} useTrash={pending.useTrash} inUse={pending.inUse} />
        </ConfirmDialog>
      )}

      {result && !pending && <ResultNote outcome={result} onClose={() => setResult(null)} />}
    </div>
  );
}

function TreeRow({
  entry: e,
  index,
  total,
  now,
  focused,
  picked,
  onFocus,
  onToggle,
  onOpen,
}: {
  entry: TreeEntry;
  index: number;
  total: number;
  now: number;
  focused: boolean;
  picked: boolean;
  onFocus: () => void;
  onToggle: () => void;
  onOpen: () => void;
}) {
  const frac = share(e.bytes, total);
  const selectable = canSelect(e);
  return (
    <div
      className={`tree-row ${focused ? "focused" : ""} ${picked ? "picked" : ""}`}
      role="row"
      aria-selected={focused}
      data-index={index}
      onClick={onFocus}
    >
      <input
        type="checkbox"
        aria-label={`Select ${e.name}`}
        checked={picked}
        disabled={!selectable}
        title={e.blocked ?? undefined}
        onChange={onToggle}
      />
      <div className="tree-name">
        {e.kind === "dir" ? (
          <button className="name-btn dir" onClick={onOpen} title={e.path}>
            <span aria-hidden>📁 </span>
            {e.name}
          </button>
        ) : (
          <span className={`name-plain ${e.kind}`} title={e.path || e.name}>
            <span aria-hidden>
              {e.kind === "collapsed" ? "⋯" : e.kind === "symlink" ? "↪" : e.kind === "other_fs" ? "💽" : "📄"}{" "}
            </span>
            {e.name}
          </span>
        )}
        {entryBadges(e).map((b) => (
          <span key={b.label} className={`badge ${b.tone}`} title={b.title}>
            {b.label}
          </span>
        ))}
      </div>
      <div className="tree-share" title={`${(frac * 100).toFixed(1)}% of this folder`}>
        <div className="bar">
          <span style={{ width: `${frac * 100}%` }} />
        </div>
        <span className="meta">{(frac * 100).toFixed(1)}%</span>
      </div>
      <span className="meta tree-items">{e.kind === "dir" || e.kind === "collapsed" ? e.items.toLocaleString() : ""}</span>
      <span className="meta tree-age">{formatAge(now, e.mtime)}</span>
      <strong className="tree-size">{formatBytesLocal(e.bytes)}</strong>
      {e.path ? (
        <button className="btn btn-ghost btn-sm" onClick={() => apiOpenPath(e.path)} aria-label={`Open ${e.name}`}>
          Open
        </button>
      ) : (
        <span />
      )}
    </div>
  );
}

function SpaceCard({ acc }: { acc: SpaceAccounting }) {
  const { fs } = acc;
  const usedFrac = share(fs.used_bytes, fs.total_bytes);
  return (
    <section className="space-card" aria-label="Disk usage">
      <div className="space-head">
        <strong>{fs.mount_point}</strong>
        <span className="meta">
          {formatBytesLocal(fs.used_bytes)} used of {formatBytesLocal(fs.total_bytes)} ·{" "}
          {formatBytesLocal(fs.available_bytes)} free
          {fs.reserved_bytes > 0 ? ` · ${formatBytesLocal(fs.reserved_bytes)} reserved for root` : ""}
        </span>
      </div>
      <div className="bar">
        <span style={{ width: `${usedFrac * 100}%` }} />
      </div>
      {acc.is_mount_root && acc.unaccounted_bytes > 0 ? (
        <div className="hidden-space">
          <p className="meta">
            The scan found {formatBytesLocal(acc.scanned_bytes)}; {formatBytesLocal(acc.unaccounted_bytes)} is used
            where a normal user can't look:
          </p>
          <ul>
            {acc.sources.map((s) => (
              <li key={s.kind}>
                <strong>{formatBytesLocal(s.bytes)}</strong> {s.label}
                {s.detail && <span className="meta"> — {s.detail}</span>}
                {s.kind === "docker" && <span className="meta"> (see the Docker tab)</span>}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        acc.sources
          .filter((s) => s.kind === "deleted_open")
          .map((s) => (
            <p className="meta" key={s.kind}>
              {formatBytesLocal(s.bytes)} {s.label.toLowerCase()} — {s.detail}
            </p>
          ))
      )}
    </section>
  );
}

function ConfirmBody({ picks, useTrash, inUse }: { picks: PickMap; useTrash: boolean; inUse: InUse[] | null }) {
  const items = topLevelPicks(picks);
  const total = items.reduce((s, [, b]) => s + b, 0);
  return (
    <div style={{ textAlign: "left" }}>
      <p>
        {items.length} item{items.length === 1 ? "" : "s"} · <strong>{formatBytesLocal(total)}</strong>
      </p>
      <ul className="confirm-list">
        {items.slice(0, 8).map(([p, b]) => (
          <li key={p}>
            <span className="path">{p}</span> <span className="meta">{formatBytesLocal(b)}</span>
          </li>
        ))}
        {items.length > 8 && <li className="meta">…and {items.length - 8} more</li>}
      </ul>
      {inUse === null ? (
        <p className="meta">Checking running processes…</p>
      ) : inUse.length > 0 ? (
        <div className="warn-box" role="alert">
          <strong>In use by running programs.</strong> Deleting may break them:
          <ul>
            {inUse.map((u) => (
              <li key={u.path}>
                <span className="path">{u.path}</span> ←{" "}
                {u.processes.map((p) => `${p.name} (${p.pid}, ${p.how})`).join(", ")}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="meta">No running program uses these paths.</p>
      )}
      <p className="meta">
        {useTrash
          ? "Trash keeps the data on disk until you empty it — space is freed only then. Items on other disks must be deleted permanently."
          : "This cannot be undone."}
      </p>
    </div>
  );
}

function ResultNote({ outcome, onClose }: { outcome: DeleteOutcome; onClose: () => void }) {
  const verb = outcome.trashed ? "Moved to Trash" : "Deleted";
  return (
    <div className={`result-note ${outcome.failures.length ? "has-failures" : ""}`} role="status">
      <span>
        {verb} {outcome.removed.length} item{outcome.removed.length === 1 ? "" : "s"} ·{" "}
        {formatBytesLocal(outcome.bytes_freed)}
        {outcome.failures.length > 0 && ` · ${outcome.failures.length} failed`}
      </span>
      {outcome.failures.length > 0 && (
        <ul>
          {outcome.failures.slice(0, 5).map((f) => (
            <li key={f.path}>
              <span className="path">{f.path}</span>: {f.error}
            </li>
          ))}
        </ul>
      )}
      <button className="btn btn-ghost btn-sm" onClick={onClose} aria-label="Dismiss">
        ✕
      </button>
    </div>
  );
}
