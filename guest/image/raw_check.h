#ifndef HAMN_IMAGE_RAW_CHECK_H
#define HAMN_IMAGE_RAW_CHECK_H

#include "image/image_util.h"

/*
 * Checks a decoded raw guest disk after decoder/reference comparison: an
 * 8 GiB virtual size, a protective MBR (0x55AA signature, type 0xEE), a GPT
 * header at LBA 1 with a valid header CRC, and a non-empty partition entry
 * array (at most 16 MiB) with a valid CRC. 512-byte logical sectors. Builds
 * with -DHAMN_TEST (tests only) accept HAMN_TEST_RAW_VIRTUAL_BYTES instead of
 * 8 GiB so orchestration tests need not hash multi-GiB fixtures.
 */
int raw_check(const char *path, struct image_error *error);

#endif
