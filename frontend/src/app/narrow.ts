/**
 * Reading a value that came from outside the app — a `catch` binding, a fetch
 * body — without asserting what it is.
 *
 * `x as Shape` is a claim, not a check: it tells the compiler what arrived and
 * then never looks. When the claim is wrong the failure surfaces far from the
 * line that made it — as `undefined` where the types promised a value, or as
 * "[object Object]" where they promised a string. Nothing in the toolchain can
 * catch that, because the assertion is the thing that lied to it.
 */

/** A value that can be indexed by string — i.e. worth asking about a field. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

/** The named field, only if it really is a non-empty string. */
export function stringField(value: unknown, key: string): string | null {
  if (!isRecord(value)) return null;
  const field = value[key];
  return typeof field === 'string' && field !== '' ? field : null;
}
