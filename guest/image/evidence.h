#ifndef HAMN_IMAGE_EVIDENCE_H
#define HAMN_IMAGE_EVIDENCE_H

/*
 * Export a verified same-build baseline without replacing existing evidence.
 *
 * Only caller-owned regular single-link sources and non-writable owned output
 * directories are accepted. Files become visible only after a full copy, hash
 * and fsync; exclusive hard links never overwrite a competing artifact. A
 * failed publish removes only links still naming this attempt's inode and its
 * private ".hamn-baseline-*" stage. The outputs are review inputs, not
 * independently approved release artifacts.
 */

#include <stddef.h>

#include "image/image_util.h"

/*
 * Test seam for the durability and publication steps. SYNC receives each
 * fsync target (stage, sidecar, output directory); LINK publishes one staged
 * file as NAME in DIRECTORY_FD without replacing an existing entry.
 */
struct evidence_io {
    int (*sync)(int fd, void *context);
    int (*link)(const char *source, int directory_fd, const char *name,
                void *context);
    void *context;
};

/*
 * Accepts DESTINATION only when its parent directory is a real directory
 * owned by the effective user without group/other write, its name is
 * non-empty without control characters, and neither DESTINATION nor
 * DESTINATION.sha256 exists or equals a RESERVED path after resolving parents.
 */
int evidence_check(const char *destination, char *const reserved[],
                   size_t reserved_count, struct image_error *error);

/*
 * Publishes SOURCE as DESTINATION plus DESTINATION.sha256 ("<hex>  <name>\n"),
 * both mode 0600 with one link, after checking SOURCE against REPORT's
 * baselineSha256 and baselineCompressedBytes. IO may be NULL for the real
 * fsync/linkat.
 */
int evidence_publish(const char *source, const char *destination,
                     const char *report, const struct evidence_io *io,
                     struct image_error *error);

#endif
