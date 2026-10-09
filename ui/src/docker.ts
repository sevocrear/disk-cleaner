import type { DockerImage } from "./types";

/** Same rule as the engine: no containers and last use older than `days`. */
export function isStale(img: DockerImage, days: number, now: number): boolean {
  if (img.containers.length) return false;
  const cutoff = now - days * 86_400;
  return img.last_used == null || img.last_used < cutoff;
}

export function staleIds(images: DockerImage[], days: number, now: number): string[] {
  return images.filter((i) => isStale(i, days, now)).map((i) => i.id);
}

export function imagesBytes(images: DockerImage[], ids: Iterable<string>): number {
  const set = new Set(ids);
  return images.filter((i) => set.has(i.id)).reduce((s, i) => s + i.bytes, 0);
}

export function imageLabel(img: DockerImage): string {
  return img.tags[0] ?? `<none> ${img.short_id}`;
}

const SOURCE_LABEL: Record<string, string> = {
  running: "running now",
  container: "container",
  tracker: "tracked run",
  pulled: "pulled / tagged",
  built: "built",
};

export function sourceLabel(src: string): string {
  return SOURCE_LABEL[src] ?? (src || "unknown");
}

/** Parse a days input; null when not a whole number >= 0. */
export function parseDays(text: string): number | null {
  const t = text.trim();
  if (!/^\d+$/.test(t)) return null;
  const n = Number(t);
  return n <= 3650 ? n : null;
}
