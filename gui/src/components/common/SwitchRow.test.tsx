import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test } from "vitest";
import { SwitchRow } from "./SwitchRow";

function renderSwitch(onChange: (checked: boolean) => Promise<unknown>) {
  render(
    <SwitchRow
      title="Keep this computer awake"
      hint="Even when nothing is playing."
      checked={false}
      disabled={false}
      onChange={onChange}
    />,
  );
  return screen.getByRole("switch", { name: "Keep this computer awake" });
}

test("shows the new value while it saves, then the saved one", async () => {
  // The saved value never changes here, as when saving fails.
  let finish!: () => void;
  const toggle = renderSwitch(
    () => new Promise<void>((resolve) => (finish = resolve)),
  );
  await userEvent.click(toggle);
  expect(toggle).toBeChecked();
  await act(async () => finish());
  expect(toggle).not.toBeChecked();
});

test("clicking the title flips it too, and the hint describes it", async () => {
  const changes: boolean[] = [];
  const toggle = renderSwitch(async (checked) => changes.push(checked));
  expect(toggle).toHaveAccessibleDescription("Even when nothing is playing.");
  await userEvent.click(screen.getByText("Keep this computer awake"));
  expect(changes).toEqual([true]);
});
