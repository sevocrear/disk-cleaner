import { useCallback, useEffect, useMemo, useState } from "react";
import {
  apiDockerInventory,
  apiDockerPruneBuildCache,
  apiDockerRemoveImages,
  apiDockerTrackerInstall,
  apiDockerTrackerUninstall,
  formatBytesLocal,
} from "./api";
import { ConfirmDialog } from "./ConfirmDialog";
import { imageLabel, imagesBytes, isStale, parseDays, sourceLabel, staleIds } from "./docker";
import { formatAge } from "./overview";
import type { ApplySummary, Config, DockerInventory } from "./types";

type Pending = { kind: "images"; ids: string[] } | { kind: "cache" };

export function DockerPanel({ config }: { config: Config }) {
  const [daysText, setDaysText] = useState(String(config.docker_unused_days || 30));
  const [days, setDays] = useState(config.docker_unused_days || 30);
  const [inv, setInv] = useState<DockerInventory | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [pending, setPending] = useState<Pending | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (d: number) => {
    setLoading(true);
    setError(null);
    try {
      const next = await apiDockerInventory(d);
      setInv(next);
      setSelected((prev) => new Set([...prev].filter((id) => next.images.some((i) => i.id === id && !i.containers.length))));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // Initial load only; later reloads happen on Apply / Refresh, not per keystroke.
    void load(days);
  }, [load]);

  const now = inv?.now ?? Math.floor(Date.now() / 1000);
  const images = inv?.images ?? [];
  const stale = useMemo(() => staleIds(images, days, now), [images, days, now]);
  const staleBytes = imagesBytes(images, stale);
  const selectedBytes = imagesBytes(images, selected);
  const daysInvalid = parseDays(daysText) === null;

  function applyDays() {
    const d = parseDays(daysText);
    if (d === null) return;
    setDays(d);
    void load(d);
  }

  function toggle(id: string, on: boolean) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  function summaryText(s: ApplySummary, what: string): string {
    return s.failures
      ? `${what}: ${s.failures} failed — see the report for details.`
      : `${what}: freed ${formatBytesLocal(s.bytes_reclaimed)}.`;
  }

  async function confirm() {
    if (!pending) return;
    setBusy(true);
    setError(null);
    try {
      if (pending.kind === "images") {
        const s = await apiDockerRemoveImages(pending.ids);
        setMessage(summaryText(s, `Removed ${pending.ids.length} image${pending.ids.length === 1 ? "" : "s"}`));
        setSelected(new Set());
      } else {
        const s = await apiDockerPruneBuildCache(days, inv?.build_cache.stale_bytes ?? 0);
        setMessage(summaryText(s, "Pruned build cache"));
      }
      await load(days);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setPending(null);
    }
  }

  async function setTracker(on: boolean) {
    setBusy(true);
    setError(null);
    try {
      const t = on ? await apiDockerTrackerInstall() : await apiDockerTrackerUninstall();
      setInv((prev) => (prev ? { ...prev, tracker: t } : prev));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const bc = inv?.build_cache;
  const tracker = inv?.tracker;

  return (
    <div className="panel docker-panel">
      <div className="panel-header" style={{ display: "flex", justifyContent: "space-between", gap: 16 }}>
        <div>
          <h2>Docker</h2>
          <p>Images and build cache by when they were last used — not just when they were built.</p>
        </div>
        <form
          className="days-form"
          onSubmit={(e) => {
            e.preventDefault();
            applyDays();
          }}
        >
          <label className="meta" htmlFor="docker-days">
            Unused for more than
          </label>
          <input
            id="docker-days"
            className="days-input"
            value={daysText}
            onChange={(e) => setDaysText(e.target.value)}
            aria-invalid={daysInvalid}
            inputMode="numeric"
          />
          <span className="meta">days</span>
          <button className="btn btn-ghost btn-sm" type="submit" disabled={daysInvalid || loading}>
            {loading ? "Loading…" : "Apply"}
          </button>
        </form>
      </div>

      {error && <div className="inline-error" role="alert">{error}</div>}
      {message && (
        <div className="result-note" role="status">
          <span>{message}</span>
          <button className="btn btn-ghost btn-sm" onClick={() => setMessage(null)} aria-label="Dismiss">
            ✕
          </button>
        </div>
      )}

      {inv && !inv.available && <div className="empty">{inv.error ?? "Docker is not available."}</div>}

      {inv?.available && bc && (
        <div className="docker-cards">
          <section className="space-card" aria-label="Build cache">
            <div className="space-head">
              <strong>Build cache</strong>
              <span className="meta">
                {bc.available
                  ? `${formatBytesLocal(bc.total_bytes)} total · ${formatBytesLocal(bc.reclaimable_bytes)} not in use`
                  : "buildx not available"}
              </span>
            </div>
            <p className="meta">
              {formatBytesLocal(bc.stale_bytes)} in {bc.stale_records} records unused for more than {days} days.
              Pruning uses BuildKit's own last-used time.
            </p>
            <div>
              <button
                className="btn btn-primary"
                disabled={busy || bc.stale_bytes <= 0}
                onClick={() => setPending({ kind: "cache" })}
              >
                Prune cache unused &gt; {days}d ({formatBytesLocal(bc.stale_bytes)})
              </button>
            </div>
          </section>

          <section className="space-card" aria-label="Usage tracker">
            <div className="space-head">
              <strong>Usage tracker</strong>
              <span className={`badge ${tracker?.active ? "ok" : "muted"}`}>
                {tracker?.active ? "on" : tracker?.installed ? "installed, stopped" : "off"}
              </span>
            </div>
            <p className="meta">
              Docker forgets when an image ran once its container is removed (<code>docker run --rm</code>). The tracker
              records every container start so "last used" stays exact.
              {tracker?.tracked_images ? ` ${tracker.tracked_images} images recorded.` : ""}
            </p>
            <div>
              {tracker?.installed ? (
                <button className="btn btn-ghost" disabled={busy} onClick={() => void setTracker(false)}>
                  Turn off
                </button>
              ) : (
                <button className="btn btn-ghost" disabled={busy} onClick={() => void setTracker(true)}>
                  Turn on (systemd user service)
                </button>
              )}
            </div>
          </section>
        </div>
      )}

      {inv?.available && (
        <>
          <div className="selection-bar">
            <button
              className="btn btn-ghost"
              disabled={!stale.length}
              onClick={() => setSelected(new Set(stale))}
            >
              Select unused &gt; {days}d ({stale.length} · {formatBytesLocal(staleBytes)})
            </button>
            <button className="btn btn-ghost" disabled={!selected.size} onClick={() => setSelected(new Set())}>
              Deselect all
            </button>
            <button className="btn btn-ghost" disabled={loading} onClick={() => void load(days)}>
              Refresh
            </button>
          </div>

          <div className="docker-table" role="table" aria-label="Docker images">
            <div className="docker-row head" role="row">
              <span />
              <span role="columnheader">Image</span>
              <span role="columnheader">Size</span>
              <span role="columnheader">Last used</span>
              <span role="columnheader">Containers</span>
            </div>
            {images.map((img) => {
              const isOld = isStale(img, days, now);
              const locked = img.containers.length > 0;
              return (
                <div className={`docker-row ${isOld ? "stale" : ""}`} role="row" key={img.id}>
                  <input
                    type="checkbox"
                    aria-label={`Select ${imageLabel(img)}`}
                    checked={selected.has(img.id)}
                    disabled={locked}
                    title={locked ? `Used by ${img.containers.join(", ")}` : undefined}
                    onChange={(e) => toggle(img.id, e.target.checked)}
                  />
                  <div className="docker-name" role="cell">
                    <strong title={img.tags.join("\n") || img.id}>{imageLabel(img)}</strong>
                    {img.tags.length > 1 && <span className="meta"> +{img.tags.length - 1} tags</span>}
                    {isOld && <span className="badge warn">unused &gt; {days}d</span>}
                    {img.running && <span className="badge ok">running</span>}
                  </div>
                  <span role="cell">{formatBytesLocal(img.bytes)}</span>
                  <span role="cell" title={sourceLabel(img.last_used_source)}>
                    {img.running ? "now" : formatAge(now, img.last_used)}
                    <span className="meta"> · {sourceLabel(img.last_used_source)}</span>
                  </span>
                  <span role="cell" className="meta">
                    {img.containers.join(", ") || "—"}
                  </span>
                </div>
              );
            })}
            {!images.length && !loading && <div className="empty">No images.</div>}
          </div>

          <div className="overview-footer">
            <div>
              <div className="meta">{selected.size} selected</div>
              <div className="display-num" style={{ fontSize: "1.5rem" }}>
                {formatBytesLocal(selectedBytes)}
              </div>
            </div>
            <button
              className="btn btn-danger"
              disabled={!selected.size || busy}
              onClick={() => setPending({ kind: "images", ids: [...selected] })}
            >
              Remove selected images
            </button>
          </div>
        </>
      )}

      {pending && (
        <ConfirmDialog
          title={pending.kind === "images" ? "Remove images?" : "Prune build cache?"}
          confirmLabel={pending.kind === "images" ? "Remove" : "Prune"}
          danger
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={() => void confirm()}
        >
          {pending.kind === "images" ? (
            <div style={{ textAlign: "left" }}>
              <p>
                {pending.ids.length} image{pending.ids.length === 1 ? "" : "s"} ·{" "}
                <strong>{formatBytesLocal(imagesBytes(images, pending.ids))}</strong>
              </p>
              <ul className="confirm-list">
                {images
                  .filter((i) => pending.ids.includes(i.id))
                  .slice(0, 8)
                  .map((i) => (
                    <li key={i.id}>
                      {imageLabel(i)} <span className="meta">{formatBytesLocal(i.bytes)}</span>
                    </li>
                  ))}
              </ul>
              <p className="meta">They can be pulled or rebuilt again later.</p>
            </div>
          ) : (
            <p>
              Remove build cache records unused for more than {days} days (≈{formatBytesLocal(bc?.stale_bytes ?? 0)}).
              Later builds may be slower once.
            </p>
          )}
        </ConfirmDialog>
      )}
    </div>
  );
}
