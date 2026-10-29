import type { FormSpec } from "./CommandDialog";
import type { Command, UnitInput } from "./types";

/** Dashboard navigation tabs. */
export type Tab =
  | "work"
  | "overview"
  | "inventory"
  | "receiving"
  | "fulfillment"
  | "locations"
  | "exceptions"
  | "activity"
  | "integrations"
  | "scanner"
  | "operations"
  | "transfers";

/** The modal currently open: a command form, an order or receipt review, or help. */
export type Modal =
  | FormSpec
  | { order: string }
  | { receipt: string }
  | { help: true }
  | null;

/** Authenticated JSON request; POST when a body is given unless a method is set. */
export type RequestJson = (
  path: string,
  body?: unknown,
  method?: "GET" | "POST" | "PUT",
) => Promise<any>;

/** Save one canonical command through the session journal; true when committed. */
export type SubmitCommand = (
  command: Command,
  expectedVersion?: number,
  capturedVersions?: Record<string, number>,
  unitInputs?: UnitInput[],
  requestId?: string,
) => Promise<boolean>;

/** Headline warehouse totals shown across tabs. */
export type Totals = {
  onHand: number;
  reserved: number;
  receiving: number;
  active: number;
};
