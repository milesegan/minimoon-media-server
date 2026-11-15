import { emit } from "@tauri-apps/api/event";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { useStore } from "../../store";
import { fakeBackend, pairingRequest, running } from "../../test/backend";
import { PairingRequests } from "./PairingRequests";

const card = (name = "Kitchen iPad wants to connect") =>
  screen.queryByRole("group", { name });

describe("pairing-request events", () => {
  test("each request shows a card once, with its code", async () => {
    fakeBackend(running());
    render(<PairingRequests />);
    // `listen` registers once its own IPC call returns.
    await act(() => new Promise((resolve) => setTimeout(resolve)));

    await act(() => emit("pairing-request", pairingRequest()));
    await act(() => emit("pairing-request", pairingRequest()));
    expect(screen.getAllByRole("group")).toHaveLength(1);
    expect(
      within(card()!).getByText(
        "Enter this code in MiniMoon on the iPadOS device.",
      ),
    ).toBeInTheDocument();
    expect(
      within(card()!).getByRole("img", { name: "Pairing code 482 913" }),
    ).toBeInTheDocument();
  });
});

describe("a card", () => {
  function showRequest() {
    fakeBackend(running());
    vi.useFakeTimers();
    useStore.getState().addPairing(pairingRequest({ expiresInSecs: 120 }));
    render(<PairingRequests />);
  }

  test("counts down once a second", () => {
    showRequest();
    expect(within(card()!).getByText("2:00 left")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1000));
    expect(within(card()!).getByText("1:59 left")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(60_000));
    expect(within(card()!).getByText("0:59 left")).toBeInTheDocument();
  });

  test("goes away when the code expires", () => {
    showRequest();
    act(() => vi.advanceTimersByTime(119_000));
    expect(card()).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1000));
    expect(card()).toBeNull();
    expect(useStore.getState().pairings).toEqual([]);
  });

  test("Dismiss takes it down", () => {
    showRequest();
    fireEvent.click(within(card()!).getByRole("button", { name: "Dismiss" }));
    expect(card()).toBeNull();
    expect(useStore.getState().pairings).toEqual([]);
  });
});
