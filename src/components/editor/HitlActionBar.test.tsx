import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { HitlActionBar } from "./HitlActionBar";

describe("HitlActionBar", () => {
  it("supports workspace review shortcuts while ignoring typing targets", async () => {
    const user = userEvent.setup();
    const onDecision = vi.fn();
    render(<><input aria-label="Review note" /><HitlActionBar busy={false} onDecision={onDecision} /></>);

    fireEvent.keyDown(screen.getByLabelText("Review note"), { key: "Enter" });
    expect(onDecision).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Enter" });
    fireEvent.keyDown(window, { key: "r" });
    await user.click(screen.getByRole("button", { name: /^Reject/ }));

    expect(onDecision.mock.calls.map(([decision]) => decision)).toEqual([
      "approve",
      "request_revision",
      "reject"
    ]);
  });

  it("disables actions and shortcuts while a decision is pending", () => {
    const onDecision = vi.fn();
    render(<HitlActionBar busy={true} onDecision={onDecision} />);

    fireEvent.keyDown(window, { key: "Enter" });
    expect(onDecision).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^Approve/ })).toBeDisabled();
  });
});
