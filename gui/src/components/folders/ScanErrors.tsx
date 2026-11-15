import { ChevronIcon } from "../icons";

/**
 * The items a folder's last scan couldn't read, behind a disclosure under its row. It's left
 * uncontrolled, so the status poll re-rendering the row doesn't close it.
 */
export function ScanErrors({ errors }: { errors: string[] }) {
  if (errors.length === 0) return null;
  const count = errors.length;
  return (
    <details className="errors">
      <summary>
        <ChevronIcon />
        <span>
          {count} {count === 1 ? "item" : "items"} couldn't be read and aren't
          shared
        </span>
      </summary>
      <ul>
        {errors.map((error, index) => (
          <li key={index}>{error}</li>
        ))}
      </ul>
    </details>
  );
}
