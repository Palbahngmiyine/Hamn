#ifndef HAMN_RAW_CACHE_H
#define HAMN_RAW_CACHE_H

/* Clone a validated, digest-addressed sparse raw base into a nonexistent path.
 * The source must be a private-user-owned managed hamn-guest-<sha256>.img.
 * Cache bundles are private, SHA-256/virtual-size/extractor-version verified,
 * fsynced and published atomically under a per-digest lock (60s deadline).
 * No network access, profile locks, existing-disk writes or VM side effects.
 * Return 0 on success and -1 on integrity, ownership, permission or I/O failure.
 * ONLY EXDEV/ENOTSUP/EOPNOTSUPP cause direct sparse extraction fallback from
 * the same hash-verified descriptor, without reopening the source pathname.
 * A successful target inherits mode 0400; the caller makes its private copy
 * writable before growing it. The caller owns and cleans up that target.
 */
int raw_cache_clone(const char *image, const char *target);

#ifdef HAMN_TEST
/* Faults are one-shot and never present in production builds. */
void raw_cache_test_fail(const char *point, int error);
#endif
#endif
