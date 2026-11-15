// Inline SVG icons. They're decorative (hidden from screen readers) and take their colour from
// CSS through currentColor.

import type { ReactNode, SVGProps } from "react";

type IconProps = Omit<SVGProps<SVGSVGElement>, "children">;

function icon(body: ReactNode, viewBox = "0 0 24 24") {
  return function Icon(props: IconProps) {
    return (
      <svg viewBox={viewBox} aria-hidden="true" {...props}>
        {body}
      </svg>
    );
  };
}

/** The crescent from the app icon, filled cream by CSS. */
export const MoonIcon = icon(
  <path d="M20 14.5A8.5 8.5 0 1 1 9.5 4a7 7 0 0 0 10.5 10.5z" />,
);

export const FolderIcon = icon(
  <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />,
);

export const PlusIcon = icon(<path d="M12 5v14M5 12h14" />);

export const CopyIcon = icon(
  <>
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M5 15V6a2 2 0 0 1 2-2h9" />
  </>,
);

/** Three dots, for a row's actions menu. */
export const MoreIcon = icon(
  <>
    <circle cx="3" cy="8" r="1.4" />
    <circle cx="8" cy="8" r="1.4" />
    <circle cx="13" cy="8" r="1.4" />
  </>,
  "0 0 16 16",
);

export const CloseIcon = icon(<path d="M6 6l12 12M18 6L6 18" />);

/** Points right; an open disclosure rotates it to point down. */
export const ChevronIcon = icon(<path d="M9 6l6 6-6 6" />);

export const TvIcon = icon(
  <>
    <rect x="3" y="5" width="18" height="12" rx="2" />
    <path d="M8 21h8" />
  </>,
);

export const ComputerIcon = icon(
  <>
    <rect x="4" y="5" width="16" height="11" rx="1.5" />
    <path d="M2 19h20" />
  </>,
);

export const TabletIcon = icon(
  <>
    <rect x="5" y="3" width="14" height="18" rx="2.5" />
    <path d="M11 18h2" />
  </>,
);

export const PhoneIcon = icon(
  <>
    <rect x="7" y="3" width="10" height="18" rx="2.5" />
    <path d="M11 18h2" />
  </>,
);
