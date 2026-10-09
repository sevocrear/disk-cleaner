import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ConfirmDialog } from "./ConfirmDialog";

describe("ConfirmDialog", () => {
  it("confirms, cancels by button, backdrop and Escape", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog title="Delete?" confirmLabel="Delete" danger onConfirm={onConfirm} onCancel={onCancel}>
        body text
      </ConfirmDialog>,
    );
    const dialog = screen.getByRole("dialog", { name: "Delete?" });
    expect(dialog).toHaveTextContent("body text");
    expect(screen.getByRole("button", { name: "Delete" })).toHaveFocus();

    await user.click(screen.getByRole("button", { name: "Delete" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await user.keyboard("{Escape}");
    await user.click(dialog.parentElement!); // backdrop
    expect(onCancel).toHaveBeenCalledTimes(3);
  });

  it("is inert while busy", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    render(
      <ConfirmDialog title="T" confirmLabel="Go" busy onConfirm={vi.fn()} onCancel={onCancel}>
        x
      </ConfirmDialog>,
    );
    expect(screen.getByRole("button", { name: "Working…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
    await user.keyboard("{Escape}");
    expect(onCancel).not.toHaveBeenCalled();
  });
});
