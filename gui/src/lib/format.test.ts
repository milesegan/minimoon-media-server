import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
  formatDate,
  formatNumber,
  formatTimeLeft,
  pairedText,
  plural,
} from "./format";

const DAY = 86_400_000;

test("formatNumber groups thousands", () => {
  expect(formatNumber(48213)).toBe("48,213");
  expect(formatNumber(0)).toBe("0");
});

describe("plural", () => {
  test("uses the singular only for exactly one", () => {
    expect(plural(1, "file")).toBe("1 file");
    expect(plural(0, "file")).toBe("0 files");
    expect(plural(48213, "file")).toBe("48,213 files");
  });

  test("takes an irregular plural", () => {
    expect(plural(2, "child", "children")).toBe("2 children");
  });
});

test("formatDate is the medium date style", () => {
  expect(formatDate(Date.UTC(2026, 7, 17, 12))).toBe("Aug 17, 2026");
});

describe("pairedText", () => {
  const now = Date.UTC(2026, 10, 15, 12);
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(now);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  test("is relative for the first month", () => {
    expect(pairedText(now - 1000)).toBe("Paired today");
    expect(pairedText(now - DAY)).toBe("Paired yesterday");
    expect(pairedText(now - 3 * DAY)).toBe("Paired 3 days ago");
    expect(pairedText(now - 29 * DAY)).toBe("Paired 29 days ago");
  });

  test("is the date from 30 days on", () => {
    expect(pairedText(now - 30 * DAY)).toBe("Paired Oct 16, 2026");
  });
});

test("formatTimeLeft is minutes and two-digit seconds", () => {
  expect(formatTimeLeft(0)).toBe("0:00");
  expect(formatTimeLeft(9)).toBe("0:09");
  expect(formatTimeLeft(94)).toBe("1:34");
  expect(formatTimeLeft(600)).toBe("10:00");
});
