type Props = {
  disabled?: boolean;
  showSized?: boolean;
  /** Disable Select all when there is nothing to select (Review). Deep clean always allows it. */
  disableSelectAll?: boolean;
  /** Disable Deselect when already empty (Review). Deep clean always allows it (clears categories). */
  disableDeselectAll?: boolean;
  onSelectSized?: () => void;
  onSelectAll: () => void;
  onDeselectAll: () => void;
};

/** Shared Select all / Deselect all (and optional Select sized) controls. */
export function SelectionBar({
  disabled = false,
  showSized = false,
  disableSelectAll = false,
  disableDeselectAll = false,
  onSelectSized,
  onSelectAll,
  onDeselectAll,
}: Props) {
  return (
    <div className="selection-bar">
      {showSized && onSelectSized && (
        <button className="btn btn-ghost" disabled={disabled || disableSelectAll} onClick={onSelectSized}>
          Select sized
        </button>
      )}
      <button className="btn btn-ghost" disabled={disabled || disableSelectAll} onClick={onSelectAll}>
        Select all
      </button>
      <button className="btn btn-ghost" disabled={disabled || disableDeselectAll} onClick={onDeselectAll}>
        Deselect all
      </button>
    </div>
  );
}
