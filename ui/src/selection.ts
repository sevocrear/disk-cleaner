import type { Action } from "./types";

/** Above this, we skip materializing every id into a Set after scan. */
export const AUTO_SELECT_LIMIT = 2_000;

const COMMAND_KINDS = new Set(["docker_cmd", "cache_cmd", "snap_remove", "flatpak_uninstall"]);

export type ItemSelection = {
  /** Explicit ids when includeAll is false. */
  ids: Set<string>;
  /** All actionable items from enabled categories are selected. */
  includeAll: boolean;
  /** Ids unchecked while includeAll is true (sparse exceptions). */
  excluded: Set<string>;
};

export function emptyItemSelection(): ItemSelection {
  return { ids: new Set(), includeAll: false, excluded: new Set() };
}

export function isCommandAction(a: Action): boolean {
  return COMMAND_KINDS.has(a.kind) || Boolean(a.command?.length);
}

/** Default-selected: sized work, plus tiny network prune when listed. */
export function isDefaultSelected(a: Action): boolean {
  return a.bytes > 0 || a.path === "network_prune";
}

export function actionable(actions: Action[]): Action[] {
  return actions.filter((a) => a.kind !== "rmdir_if_empty");
}

export function selectSizedIds(actions: Action[]): Set<string> {
  return new Set(actionable(actions).filter(isDefaultSelected).map((a) => a.id));
}

/** After scan: sized ids when small; includeAll when large (Move to Trash ready immediately). */
export function defaultItemSelection(actions: Action[]): ItemSelection {
  const list = actionable(actions);
  if (!list.length) return emptyItemSelection();
  const pick = selectSizedIds(actions);
  if (pick.size > AUTO_SELECT_LIMIT) return selectionWithAll();
  if (pick.size > 0) return { ids: pick, includeAll: false, excluded: new Set() };
  // Only zero-byte / non-sized candidates — still select them so Clean is usable.
  return selectionWithAll();
}

/** True when nothing is selected (footer 0 / Move to Trash disabled). */
export function isSelectionEmpty(sel: ItemSelection): boolean {
  return !sel.includeAll && sel.ids.size === 0;
}

/**
 * Enabling a Deep-clean category must make its items cleanable.
 * If selection was empty or a Review subset, switch to includeAll so the
 * newly enabled category is included without requiring Select all.
 */
export function selectionAfterEnablingCategory(prev: ItemSelection): ItemSelection {
  if (prev.includeAll) return prev;
  return selectionWithAll();
}

export function selectionWithAll(): ItemSelection {
  return { ids: new Set(), includeAll: true, excluded: new Set() };
}

export function selectionWithSized(actions: Action[]): ItemSelection {
  return { ids: selectSizedIds(actions), includeAll: false, excluded: new Set() };
}

export function resolveSelectedActions(allActions: Action[], sel: ItemSelection): Action[] {
  if (sel.includeAll) {
    if (!sel.excluded.size) return allActions;
    return allActions.filter((a) => !sel.excluded.has(a.id));
  }
  if (!sel.ids.size) return [];
  return allActions.filter((a) => sel.ids.has(a.id));
}

export function isItemSelected(id: string, sel: ItemSelection): boolean {
  if (sel.includeAll) return !sel.excluded.has(id);
  return sel.ids.has(id);
}

export function selectedCount(allActions: Action[], sel: ItemSelection): number {
  if (sel.includeAll) return Math.max(0, allActions.length - sel.excluded.size);
  let n = 0;
  for (const a of allActions) if (sel.ids.has(a.id)) n += 1;
  return n;
}

/** Toggle one row in Review; keeps includeAll + excluded for huge lists. */
export function toggleItemSelection(sel: ItemSelection, id: string, checked: boolean): ItemSelection {
  if (sel.includeAll) {
    const excluded = new Set(sel.excluded);
    if (checked) excluded.delete(id);
    else excluded.add(id);
    return { ids: sel.ids, includeAll: true, excluded };
  }
  const ids = new Set(sel.ids);
  if (checked) ids.add(id);
  else ids.delete(id);
  return { ids, includeAll: false, excluded: sel.excluded };
}

/** Drop explicit ids that no longer exist in enabled categories. */
export function pruneItemSelection(sel: ItemSelection, allowedIds: Set<string>): ItemSelection {
  if (sel.includeAll) {
    if (!sel.excluded.size) return sel;
    let changed = false;
    const excluded = new Set<string>();
    for (const id of sel.excluded) {
      if (allowedIds.has(id)) excluded.add(id);
      else changed = true;
    }
    return changed ? { ...sel, excluded } : sel;
  }
  let changed = false;
  const ids = new Set<string>();
  for (const id of sel.ids) {
    if (allowedIds.has(id)) ids.add(id);
    else changed = true;
  }
  return changed ? { ids, includeAll: false, excluded: sel.excluded } : sel;
}

export function cleanButtonLabel(_actions: Action[], useTrash: boolean, busy: boolean): string {
  if (busy) return "Cleaning…";
  return useTrash ? "Move to Trash" : "Clean";
}

export function canClean(selectedCount: number, busy: boolean, scanning: boolean): boolean {
  return selectedCount > 0 && !busy && !scanning;
}
