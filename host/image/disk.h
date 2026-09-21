#ifndef HAMN_DISK_H
#define HAMN_DISK_H

#include "core/profile.h"

/* Prepare <profile>/disk.img while the caller owns its lifecycle lock. New
 * disks use a verified digest raw base and APFS CoW, with direct sparse
 * extraction only on unsupported clone filesystems. Publish after fsync via
 * a private sibling stage. Existing regular, owned, unshared disks are only
 * grown to disk_gib (GiB); they are never replaced or rebased. Symlink,
 * permission and integrity errors fail closed. Returns 0 or -1 with errno.
 */
int disk_prepare(const struct profile *p, const char *cache_img);

#endif
