#ifndef HAMN_IMAGE_DIGEST_H
#define HAMN_IMAGE_DIGEST_H

/*
 * Portable digests for the Linux image builder and macOS publication checks,
 * which share no crypto library: FIPS 180-4 SHA-256 and the CRC-32 used by
 * GPT (IEEE 802.3, reflected, same results as zlib's crc32).
 */

#include <stddef.h>
#include <stdint.h>

#include "image/image_util.h"

#define SHA256_DIGEST_BYTES 32
#define SHA256_HEX_CAP 65

struct sha256 {
    uint32_t state[8];
    uint64_t total_bytes;
    unsigned char block[64];
    size_t used;
};

void sha256_init(struct sha256 *context);
void sha256_update(struct sha256 *context, const void *bytes, size_t length);
void sha256_final(struct sha256 *context,
                  unsigned char digest[SHA256_DIGEST_BYTES]);
void sha256_hex(const unsigned char digest[SHA256_DIGEST_BYTES],
                char hex[SHA256_HEX_CAP]);
/* Hashes the file PATH names (following symlinks) into lowercase HEX. */
int sha256_file(const char *path, char hex[SHA256_HEX_CAP],
                struct image_error *error);

/* crc32_ieee(0, data, n) equals zlib.crc32(data); pass a previous CRC to extend. */
uint32_t crc32_ieee(uint32_t crc, const void *bytes, size_t length);

#endif
