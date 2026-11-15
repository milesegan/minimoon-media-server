// Numbers, counts and dates as the window shows them, in the system's locale.

const numbers = new Intl.NumberFormat();
const dates = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

/** "48,213". */
export function formatNumber(value: number): string {
  return numbers.format(value);
}

/** "1 file", "48,213 files". */
export function plural(count: number, one: string, many = `${one}s`): string {
  return `${numbers.format(count)} ${count === 1 ? one : many}`;
}

/** "Aug 17, 2026". `time` is milliseconds since the Unix epoch. */
export function formatDate(time: number): string {
  return dates.format(new Date(time));
}

/** "Paired today", "Paired 3 days ago", or the date once it's a month or more ago. */
export function pairedText(pairedAt: number): string {
  const days = Math.floor((Date.now() - pairedAt) / 86_400_000);
  return days < 30
    ? `Paired ${relative.format(-days, "day")}`
    : `Paired ${formatDate(pairedAt)}`;
}

/** "1:34", for a countdown. */
export function formatTimeLeft(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
