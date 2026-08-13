#include "image/qcow2.h"

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zlib.h>

#define QCOW2_MAGIC        0x514649fbu /* "QFI\xfb" */
#define ENTRY_OFFSET_MASK  0x00fffffffffffe00ULL
#define L2E_COMPRESSED     (1ULL << 62)
#define L2E_ZERO           1ULL

static uint32_t rbe32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) |
           ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static uint64_t rbe64(const uint8_t *p)
{
    return ((uint64_t)rbe32(p) << 32) | rbe32(p + 4);
}

static int set_err(char **err, const char *fmt, ...)
    __attribute__((format(printf, 2, 3)));

static int set_err(char **err, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    if (vasprintf(err, fmt, ap) < 0)
        *err = NULL;
    va_end(ap);
    return -1;
}

static int full_pread(int fd, void *buf, size_t n, uint64_t off,
                      uint64_t fsize, char **err)
{
    if (off > fsize || n > fsize - off)
        return set_err(err, "qcow2: read beyond end of file "
                       "(off=%" PRIu64 " len=%zu size=%" PRIu64 ")",
                       off, n, fsize);
    uint8_t *p = buf;
    while (n > 0) {
        ssize_t r = pread(fd, p, n, (off_t)off);
        if (r < 0) {
            if (errno == EINTR)
                continue;
            return set_err(err, "qcow2: pread: %s", strerror(errno));
        }
        if (r == 0)
            return set_err(err, "qcow2: unexpected EOF");
        p += r;
        n -= (size_t)r;
        off += (uint64_t)r;
    }
    return 0;
}

static int full_pwrite(int fd, const void *buf, size_t n, uint64_t off,
                       char **err)
{
    const uint8_t *p = buf;
    while (n > 0) {
        ssize_t r = pwrite(fd, p, n, (off_t)off);
        if (r < 0) {
            if (errno == EINTR)
                continue;
            return set_err(err, "qcow2: pwrite: %s", strerror(errno));
        }
        p += r;
        n -= (size_t)r;
        off += (uint64_t)r;
    }
    return 0;
}

/* deflate 압축 클러스터를 풀어 out(cluster_size)에 채운다. */
static int inflate_cluster(const uint8_t *in, size_t in_len, uint8_t *out,
                           size_t out_len, char **err)
{
    z_stream zs;
    memset(&zs, 0, sizeof(zs));
    /* qcow2 압축 데이터는 raw deflate 스트림 (zlib 헤더 없음) */
    if (inflateInit2(&zs, -15) != Z_OK)
        return set_err(err, "qcow2: inflateInit2 failed");

    zs.next_in = (Bytef *)in;
    zs.avail_in = (uInt)in_len;
    zs.next_out = out;
    zs.avail_out = (uInt)out_len;

    int zr = inflate(&zs, Z_FINISH);
    int ok = (zr == Z_STREAM_END) ||
             ((zr == Z_OK || zr == Z_BUF_ERROR) && zs.avail_out == 0);
    size_t produced = out_len - zs.avail_out;
    inflateEnd(&zs);

    if (!ok || produced != out_len)
        return set_err(err, "qcow2: bad compressed cluster "
                       "(zr=%d produced=%zu want=%zu)", zr, produced, out_len);
    return 0;
}

int qcow2_extract(const char *in_path, const char *out_path, char **err)
{
    int rc = -1;
    int in_fd = -1, out_fd = -1;
    uint8_t *l1 = NULL, *l2 = NULL, *data = NULL, *zero = NULL, *cbuf = NULL;
    *err = NULL;

    in_fd = open(in_path, O_RDONLY);
    if (in_fd < 0) {
        set_err(err, "open %s: %s", in_path, strerror(errno));
        goto out;
    }
    struct stat st;
    if (fstat(in_fd, &st) != 0) {
        set_err(err, "fstat %s: %s", in_path, strerror(errno));
        goto out;
    }
    uint64_t fsize = (uint64_t)st.st_size;

    uint8_t hdr[112];
    memset(hdr, 0, sizeof(hdr));
    if (fsize < 72) {
        set_err(err, "qcow2: file too small");
        goto out;
    }
    if (full_pread(in_fd, hdr, fsize >= 112 ? 112 : 72, 0, fsize, err))
        goto out;

    if (rbe32(hdr + 0) != QCOW2_MAGIC) {
        set_err(err, "qcow2: bad magic (not a qcow2 image)");
        goto out;
    }
    uint32_t version = rbe32(hdr + 4);
    if (version != 2 && version != 3) {
        set_err(err, "qcow2: unsupported version %u", version);
        goto out;
    }
    if (rbe64(hdr + 8) != 0) {
        set_err(err, "qcow2: backing file not supported");
        goto out;
    }
    uint32_t cluster_bits = rbe32(hdr + 20);
    if (cluster_bits < 9 || cluster_bits > 22) {
        set_err(err, "qcow2: implausible cluster_bits %u", cluster_bits);
        goto out;
    }
    uint64_t size = rbe64(hdr + 24);
    if (rbe32(hdr + 32) != 0) {
        set_err(err, "qcow2: encrypted image not supported");
        goto out;
    }
    uint32_t l1_size = rbe32(hdr + 36);
    uint64_t l1_off = rbe64(hdr + 40);

    if (version == 3) {
        uint64_t incompat = rbe64(hdr + 72);
        if (incompat != 0) {
            set_err(err, "qcow2: unsupported incompatible features "
                    "0x%" PRIx64 " (extended L2 / external data / zstd?)",
                    incompat);
            goto out;
        }
    }

    size_t cluster_size = (size_t)1 << cluster_bits;
    uint64_t l2_entries = cluster_size / 8;

    /* L1 크기 정합성: 가상 크기를 커버할 만큼은 있어야 한다 */
    uint64_t need_l1 =
        (size + ((uint64_t)cluster_size * l2_entries) - 1) /
        ((uint64_t)cluster_size * l2_entries);
    if (l1_size < need_l1) {
        set_err(err, "qcow2: L1 table too small (%u < %" PRIu64 ")",
                l1_size, need_l1);
        goto out;
    }

    l1 = malloc((size_t)l1_size * 8);
    l2 = malloc(cluster_size);
    data = malloc(cluster_size);
    zero = calloc(1, cluster_size);
    /* 압축 클러스터는 드물게 cluster_size를 약간 넘을 수 있다 */
    size_t cbuf_cap = cluster_size + 4096;
    cbuf = malloc(cbuf_cap);
    if (!l1 || !l2 || !data || !zero || !cbuf) {
        set_err(err, "qcow2: out of memory");
        goto out;
    }
    if (l1_size > 0 &&
        full_pread(in_fd, l1, (size_t)l1_size * 8, l1_off, fsize, err))
        goto out;

    out_fd = open(out_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (out_fd < 0) {
        set_err(err, "open %s: %s", out_path, strerror(errno));
        goto out;
    }
    if (ftruncate(out_fd, (off_t)size) != 0) {
        set_err(err, "ftruncate %s: %s", out_path, strerror(errno));
        goto out;
    }

    uint64_t written = 0;
    for (uint32_t i = 0; i < l1_size; i++) {
        uint64_t l1e = rbe64(l1 + (size_t)i * 8);
        uint64_t l2_off = l1e & ENTRY_OFFSET_MASK;
        if (l2_off == 0)
            continue; /* 이 L2 범위 전체가 미할당 → sparse hole */
        if (full_pread(in_fd, l2, cluster_size, l2_off, fsize, err))
            goto out;

        for (uint64_t j = 0; j < l2_entries; j++) {
            uint64_t voff = ((uint64_t)i * l2_entries + j) * cluster_size;
            if (voff >= size)
                break;
            size_t want = cluster_size;
            if (size - voff < want)
                want = (size_t)(size - voff);

            uint64_t e = rbe64(l2 + (size_t)j * 8);
            if (e & L2E_COMPRESSED) {
                unsigned x = 62 - (cluster_bits - 8);
                uint64_t coff = e & ((1ULL << x) - 1);
                uint64_t nsec = ((e >> x) & ((1ULL << (62 - x)) - 1)) + 1;
                uint64_t csize64 = nsec * 512 - (coff & 511);
                if (csize64 > cbuf_cap) {
                    set_err(err, "qcow2: compressed cluster too large "
                            "(%" PRIu64 ")", csize64);
                    goto out;
                }
                size_t csize = (size_t)csize64;
                if (full_pread(in_fd, cbuf, csize, coff, fsize, err))
                    goto out;
                if (inflate_cluster(cbuf, csize, data, cluster_size, err))
                    goto out;
                if (full_pwrite(out_fd, data, want, voff, err))
                    goto out;
                written += want;
            } else {
                uint64_t hoff = e & ENTRY_OFFSET_MASK;
                if ((e & L2E_ZERO) || hoff == 0)
                    continue; /* 제로/미할당 클러스터 → hole */
                if (full_pread(in_fd, data, want, hoff, fsize, err))
                    goto out;
                if (memcmp(data, zero, want) == 0)
                    continue; /* 내용이 0이면 기록 생략 (sparse 유지) */
                if (full_pwrite(out_fd, data, want, voff, err))
                    goto out;
                written += want;
            }
        }
    }

    if (fsync(out_fd) != 0) {
        set_err(err, "fsync %s: %s", out_path, strerror(errno));
        goto out;
    }
    fprintf(stderr,
            "qcow2: extracted %s -> %s (virtual %" PRIu64 " MiB, "
            "written %" PRIu64 " MiB)\n",
            in_path, out_path, size >> 20, written >> 20);
    rc = 0;

out:
    free(l1);
    free(l2);
    free(data);
    free(zero);
    free(cbuf);
    if (in_fd >= 0)
        close(in_fd);
    if (out_fd >= 0)
        close(out_fd);
    if (rc != 0)
        unlink(out_path);
    return rc;
}
