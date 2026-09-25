#include "image/digest.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define DIGEST_READ_BYTES (1024 * 1024)

static const uint32_t sha256_constants[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static uint32_t rotate_right(uint32_t value, unsigned bits)
{
    return (value >> bits) | (value << (32 - bits));
}

static void sha256_block(struct sha256 *context, const unsigned char *block)
{
    uint32_t w[64];
    for (unsigned index = 0; index < 16; index++) {
        w[index] = (uint32_t)block[index * 4] << 24 |
                   (uint32_t)block[index * 4 + 1] << 16 |
                   (uint32_t)block[index * 4 + 2] << 8 |
                   (uint32_t)block[index * 4 + 3];
    }
    for (unsigned index = 16; index < 64; index++) {
        uint32_t s0 = rotate_right(w[index - 15], 7) ^
                      rotate_right(w[index - 15], 18) ^ (w[index - 15] >> 3);
        uint32_t s1 = rotate_right(w[index - 2], 17) ^
                      rotate_right(w[index - 2], 19) ^ (w[index - 2] >> 10);
        w[index] = w[index - 16] + s0 + w[index - 7] + s1;
    }
    uint32_t a = context->state[0], b = context->state[1];
    uint32_t c = context->state[2], d = context->state[3];
    uint32_t e = context->state[4], f = context->state[5];
    uint32_t g = context->state[6], h = context->state[7];
    for (unsigned index = 0; index < 64; index++) {
        uint32_t s1 = rotate_right(e, 6) ^ rotate_right(e, 11) ^
                      rotate_right(e, 25);
        uint32_t choose = (e & f) ^ (~e & g);
        uint32_t t1 = h + s1 + choose + sha256_constants[index] + w[index];
        uint32_t s0 = rotate_right(a, 2) ^ rotate_right(a, 13) ^
                      rotate_right(a, 22);
        uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
        uint32_t t2 = s0 + majority;
        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }
    context->state[0] += a;
    context->state[1] += b;
    context->state[2] += c;
    context->state[3] += d;
    context->state[4] += e;
    context->state[5] += f;
    context->state[6] += g;
    context->state[7] += h;
}

void sha256_init(struct sha256 *context)
{
    static const uint32_t initial[8] = {
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    };
    memcpy(context->state, initial, sizeof(initial));
    context->total_bytes = 0;
    context->used = 0;
}

void sha256_update(struct sha256 *context, const void *bytes, size_t length)
{
    const unsigned char *input = bytes;
    context->total_bytes += length;
    while (length > 0) {
        size_t take = sizeof(context->block) - context->used;
        if (take > length)
            take = length;
        memcpy(context->block + context->used, input, take);
        context->used += take;
        input += take;
        length -= take;
        if (context->used == sizeof(context->block)) {
            sha256_block(context, context->block);
            context->used = 0;
        }
    }
}

void sha256_final(struct sha256 *context,
                  unsigned char digest[SHA256_DIGEST_BYTES])
{
    uint64_t bits = context->total_bytes * 8;
    unsigned char padding[72] = { 0x80 };
    size_t pad = context->used < 56 ? 56 - context->used :
                                      120 - context->used;
    unsigned char length[8];
    for (unsigned index = 0; index < 8; index++)
        length[index] = (unsigned char)(bits >> (56 - index * 8));
    uint64_t total = context->total_bytes;
    sha256_update(context, padding, pad);
    sha256_update(context, length, sizeof(length));
    context->total_bytes = total;
    for (unsigned index = 0; index < 8; index++) {
        digest[index * 4] = (unsigned char)(context->state[index] >> 24);
        digest[index * 4 + 1] = (unsigned char)(context->state[index] >> 16);
        digest[index * 4 + 2] = (unsigned char)(context->state[index] >> 8);
        digest[index * 4 + 3] = (unsigned char)context->state[index];
    }
}

void sha256_hex(const unsigned char digest[SHA256_DIGEST_BYTES],
                char hex[SHA256_HEX_CAP])
{
    static const char digits[] = "0123456789abcdef";
    for (unsigned index = 0; index < SHA256_DIGEST_BYTES; index++) {
        hex[index * 2] = digits[digest[index] >> 4];
        hex[index * 2 + 1] = digits[digest[index] & 0x0F];
    }
    hex[SHA256_DIGEST_BYTES * 2] = '\0';
}

int sha256_file(const char *path, char hex[SHA256_HEX_CAP],
                struct image_error *error)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return image_fail_errno(error, "cannot open", path);
    unsigned char *buffer = malloc(DIGEST_READ_BYTES);
    if (!buffer) {
        close(fd);
        return image_fail(error, "out of memory hashing %s", path);
    }
    struct sha256 context;
    sha256_init(&context);
    for (;;) {
        ssize_t count = read(fd, buffer, DIGEST_READ_BYTES);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            int saved = errno;
            free(buffer);
            close(fd);
            errno = saved;
            return image_fail_errno(error, "cannot read", path);
        }
        if (count == 0)
            break;
        sha256_update(&context, buffer, (size_t)count);
    }
    free(buffer);
    close(fd);
    unsigned char digest[SHA256_DIGEST_BYTES];
    sha256_final(&context, digest);
    sha256_hex(digest, hex);
    return 0;
}

uint32_t crc32_ieee(uint32_t crc, const void *bytes, size_t length)
{
    static uint32_t table[256];
    static int ready;
    if (!ready) {
        for (uint32_t index = 0; index < 256; index++) {
            uint32_t value = index;
            for (unsigned bit = 0; bit < 8; bit++)
                value = value & 1 ? 0xEDB88320u ^ (value >> 1) : value >> 1;
            table[index] = value;
        }
        ready = 1;
    }
    const unsigned char *input = bytes;
    crc = ~crc;
    for (size_t index = 0; index < length; index++)
        crc = table[(crc ^ input[index]) & 0xFF] ^ (crc >> 8);
    return ~crc;
}
