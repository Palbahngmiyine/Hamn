#ifndef HAMN_IMAGE_SIZE_GATE_H
#define HAMN_IMAGE_SIZE_GATE_H

/*
 * Guest image size evidence and its publication boundary.
 *
 * size_gate_run records real same-build bytes and enforces the reviewed
 * compressed-size budget. Review-only mode emits a budget proposal, never a
 * reviewed budget; distribution builds require release-size-budget.json
 * committed after reviewing actual image/runtime reports. No default or
 * synthetic byte limit substitutes for that first result.
 *
 * size_release_verify is the fail-closed publication check for an actual
 * image and its size evidence.
 */

#include "image/image_util.h"

struct size_gate_options {
    const char *baseline;
    const char *candidate;
    const char *packages_before;
    const char *packages_after;
    const char *report;
    const char *budget;
    const char *base_sha256;      /* 64 lowercase hex */
    const char *source_revision;  /* 40 lowercase hex */
    int review_only;
};

/*
 * Writes OPTIONS->report (indent-2 JSON, schemaVersion 1) once the inputs are
 * valid, then rejects a candidate at or above the 2 GiB release asset limit,
 * savings below max(64 MiB, 5%) of the baseline, and (unless review-only) a
 * missing, invalid or exceeded budget. Review-only writes the proposal next to
 * the report: the report's last suffix is replaced by
 * ".budget-proposal.json".
 */
int size_gate_run(const struct size_gate_options *options,
                  struct image_error *error);

/*
 * Reads a dpkg-query inventory (package TAB version TAB installed-KiB rows).
 * guestfish's additional trailing newline is the only allowed empty line.
 * Empty inventories, malformed rows and repeated packages fail. On success
 * *ROWS is a JSON array of the unchanged row strings.
 */
int size_package_inventory(const char *path, struct json_value **rows,
                           struct image_error *error);

/* The committed budget schema: exactly four keys, integer bound, digests. */
int size_budget_validate(const struct json_value *budget,
                         struct image_error *error);

/*
 * Accepts IMAGE only when REPORT is non-review-only evidence of exactly those
 * bytes within BUDGET. EXPECTED_REVISION, when not NULL, must equal the
 * report's sourceRevision.
 */
int size_release_verify(const char *image, const char *report,
                        const char *budget, const char *expected_revision,
                        struct image_error *error);

#endif
