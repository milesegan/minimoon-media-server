import { useId, type ReactNode } from "react";

interface SectionProps {
  title: string;
  /** Goes at the right of the heading, like an Add button. */
  action?: ReactNode;
  children: ReactNode;
}

/** A titled group of the window: a heading, then a card holding its rows. */
export function Section({ title, action, children }: SectionProps) {
  const titleId = useId();
  return (
    <section aria-labelledby={titleId}>
      <div className="section-head">
        <h2 id={titleId}>{title}</h2>
        {action}
      </div>
      <div className="card">{children}</div>
    </section>
  );
}
