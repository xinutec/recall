/**
 * Reading a value from outside the app (a `catch` binding, a fetch body) by
 * checking it, not asserting it: `x as Shape` is never checked, so a wrong
 * guess fails far from where it was made.
 */

/** A value that can be indexed by string. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

/** The named field, if it is a non-empty string. */
export function stringField(value: unknown, key: string): string | null {
  if (!isRecord(value)) return null;
  const field = value[key];
  return typeof field === 'string' && field !== '' ? field : null;
}
