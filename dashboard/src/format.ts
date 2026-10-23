/** Whole numbers with thousands separators, for display only. */
export const fmt = (n: number) => new Intl.NumberFormat("en-US").format(n);

/** A short local date and time, for display only. */
export const when = (s: string) =>
  new Date(s).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
