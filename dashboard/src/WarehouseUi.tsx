import type { ReactNode } from "react";
import { Box } from "lucide-react";

export const label = (s: string) =>
  s === "closed_partial"
    ? "Part shipped · remainder cancelled"
    : s.replaceAll("_", " ").replaceAll(".", " ");

export function Badge({
  children,
  tone = "",
}: {
  children: ReactNode;
  tone?: string;
}) {
  return <span className={`badge ${tone}`}>{children}</span>;
}
export function Status({ value }: { value: string }) {
  return (
    <Badge
      tone={
        ["shipped", "received", "resolved", "delivered"].includes(value)
          ? "green"
          : [
                "pending",
                "expected",
                "partial",
                "allocated",
                "picking",
                "partially_shipped",
                "partially_shipped",
              ].includes(value)
            ? "amber"
            : ["open", "dead_letter"].includes(value)
              ? "red"
              : "blue"
      }
    >
      <i />
      {label(value)}
    </Badge>
  );
}
export function Empty({
  title,
  detail,
  action,
}: {
  title: string;
  detail: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      <Box size={30} />
      <h3>{title}</h3>
      <p>{detail}</p>
      {action}
    </div>
  );
}
export function Section({
  title,
  detail,
  action,
  children,
  className = "",
}: {
  title: string;
  detail?: string;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`panel ${className}`}>
      <div className="panel-head">
        <div>
          <h2>{title}</h2>
          {detail && <p>{detail}</p>}
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}


export function Metric({
  label: title,
  value,
  icon: Icon,
  note,
  attention,
  tone = "green",
}: {
  label: string;
  value: string;
  icon: typeof Box;
  note: string;
  attention?: boolean;
  tone?: "green" | "blue" | "purple";
}) {
  return (
    <div className={`metric metric-${tone} ${attention ? "attention" : ""}`}>
      <div>
        <span>{title}</span>
        <span className="metric-icon"><Icon size={18} /></span>
      </div>
      <strong>{value}</strong>
      <p>
        {attention ? (
          <span className="amber-dot" />
        ) : (
          <span className="metric-line" />
        )}
        {note}
      </p>
    </div>
  );
}
