import { useId, useState } from "react";

interface SwitchRowProps {
  title: string;
  hint: string;
  /** The saved setting. */
  checked: boolean;
  disabled: boolean;
  /** Saves a new value; the switch shows it until this settles. */
  onChange: (checked: boolean) => Promise<unknown>;
}

/**
 * A setting row with a title and hint on the left and a switch on the right. The whole row is
 * the label, so clicking the text flips the switch too.
 */
export function SwitchRow({
  title,
  hint,
  checked,
  disabled,
  onChange,
}: SwitchRowProps) {
  const titleId = useId();
  const hintId = useId();
  // While a change is saving, show where the switch was moved to rather than the old value.
  const [pendingValue, setPendingValue] = useState<boolean | null>(null);

  async function toggle(next: boolean) {
    setPendingValue(next);
    try {
      await onChange(next);
    } finally {
      setPendingValue(null);
    }
  }

  return (
    <label className="setting">
      <span className="setting-text">
        <span id={titleId} className="setting-title">
          {title}
        </span>
        <span id={hintId} className="hint">
          {hint}
        </span>
      </span>
      <input
        className="switch"
        type="checkbox"
        role="switch"
        aria-labelledby={titleId}
        aria-describedby={hintId}
        checked={pendingValue ?? checked}
        disabled={disabled}
        onChange={(event) => toggle(event.currentTarget.checked)}
      />
    </label>
  );
}
