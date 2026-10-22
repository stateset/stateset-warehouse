import type { Journal } from "./types";
import { sameVersions } from "./concurrency";

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
    public commandOutcome?: unknown,
  ) {
    super(message);
  }
}
export function provesStaleCommandDidNotCommit(error: ApiError, journal: Journal) {
  const outcome = error.commandOutcome as Record<string, unknown> | null;
  const identityMatches =
    error.status === 409 &&
    !!outcome &&
    typeof outcome === "object" &&
    !Array.isArray(outcome) &&
    outcome.status === "not_committed" &&
    outcome.tenant === journal.tenant &&
    outcome.actor === journal.actor &&
    outcome.warehouse_id === journal.warehouse &&
    outcome.request_id === journal.request.request_id &&
    outcome.expected_version === journal.request.expected_version &&
    Number.isSafeInteger(outcome.expected_version) &&
    Number.isSafeInteger(outcome.current_version) &&
    (outcome.current_version as number) >= journal.request.expected_version;
  if (!identityMatches || !outcome) return false;
  if (!journal.request.resource_versions)
    return (
      Object.keys(outcome).length === 8 &&
      outcome.reason === "stale_version" &&
      (outcome.current_version as number) > journal.request.expected_version
    );
  if (
    Object.keys(outcome).length !== 10 ||
    outcome.reason !== "changed_resources" ||
    !sameVersions(
      outcome.expected_resource_versions,
      journal.request.resource_versions,
    )
  )
    return false;
  const current = outcome.current_resource_versions as Record<string, unknown>;
  const expected = journal.request.resource_versions;
  if (
    !current ||
    typeof current !== "object" ||
    Array.isArray(current) ||
    Object.keys(current).length !== Object.keys(expected).length
  )
    return false;
  return (
    Object.entries(expected).every(
      ([k, v]) =>
        Number.isSafeInteger(current[k]) && (current[k] as number) >= v,
    ) && Object.entries(expected).some(([k, v]) => (current[k] as number) > v)
  );
}
