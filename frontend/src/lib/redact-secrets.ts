// Matches fields that hold credentials. Deliberately narrow so settings such as
// `maxTokens` stay readable in logs.
const SECRET_KEY = /(api[-_]?key|secret|password|auth[-_]?token|access[-_]?token)/i;

/** Copy of `value` that is safe to log: credential fields are masked. */
export function redactSecrets<T>(value: T): T {
  if (Array.isArray(value)) {
    return value.map(item => redactSecrets(item)) as unknown as T;
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([key, child]) => [
        key,
        SECRET_KEY.test(key) && child ? '***' : redactSecrets(child),
      ]),
    ) as T;
  }
  return value;
}
