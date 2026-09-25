#include "image/raw_check.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "image/digest.h"

#define SECTOR_BYTES 512
#define GPT_HEADER_MINIMUM 92
#define GPT_ENTRY_MINIMUM 128
#define GPT_TABLE_MAXIMUM (16u * 1024 * 1024)

static uint32_t le32(const unsigned char *bytes)
{
    return (uint32_t)bytes[0] | (uint32_t)bytes[1] << 8 |
           (uint32_t)bytes[2] << 16 | (uint32_t)bytes[3] << 24;
}

static uint64_t le64(const unsigned char *bytes)
{
    return (uint64_t)le32(bytes) | (uint64_t)le32(bytes + 4) << 32;
}

/* Reads up to LENGTH bytes at OFFSET; returns the count, or -1 on error. */
static ssize_t read_at(int fd, void *buffer, size_t length, off_t offset)
{
    size_t used = 0;
    while (used < length) {
        ssize_t count = pread(fd, (char *)buffer + used, length - used,
                              offset + (off_t)used);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (count == 0)
            break;
        used += (size_t)count;
    }
    return (ssize_t)used;
}

static long long virtual_bytes(void)
{
#ifdef HAMN_TEST
    /* Test builds only: small raw fixtures for orchestration tests. */
    const char *value = getenv("HAMN_TEST_RAW_VIRTUAL_BYTES");
    if (value && *value)
        return strtoll(value, NULL, 10);
#endif
    return IMAGE_VIRTUAL_BYTES;
}

int raw_check(const char *path, struct image_error *error)
{
    const long long disk_bytes = virtual_bytes();
    struct stat info;
    if (stat(path, &info) != 0)
        return image_fail_errno(error, "cannot inspect", path);
    if ((long long)info.st_size != disk_bytes)
        return image_fail(error, "guest raw virtual size must be 8 GiB");
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return image_fail_errno(error, "cannot open", path);
    unsigned char mbr[SECTOR_BYTES], header[SECTOR_BYTES];
    unsigned char *table = NULL;
    int rc = -1;
    if (read_at(fd, mbr, sizeof(mbr), 0) != (ssize_t)sizeof(mbr) ||
        read_at(fd, header, sizeof(header), SECTOR_BYTES) !=
            (ssize_t)sizeof(header)) {
        image_fail_errno(error, "cannot read", path);
        goto done;
    }
    if (mbr[510] != 0x55 || mbr[511] != 0xAA || mbr[450] != 0xEE) {
        image_fail(error, "invalid protective MBR");
        goto done;
    }
    uint32_t header_size = le32(header + 12);
    uint32_t header_crc = le32(header + 16);
    if (memcmp(header, "EFI PART", 8) != 0 ||
        header_size < GPT_HEADER_MINIMUM || header_size > SECTOR_BYTES) {
        image_fail(error, "invalid GPT header");
        goto done;
    }
    memset(header + 16, 0, 4);
    if (crc32_ieee(0, header, header_size) != header_crc) {
        image_fail(error, "invalid GPT header CRC");
        goto done;
    }
    uint64_t table_lba = le64(header + 72);
    uint32_t entries = le32(header + 80);
    uint32_t entry_size = le32(header + 84);
    uint32_t entries_crc = le32(header + 88);
    uint64_t table_size = (uint64_t)entries * entry_size;
    if (entries == 0 || entry_size < GPT_ENTRY_MINIMUM ||
        table_size > GPT_TABLE_MAXIMUM) {
        image_fail(error, "invalid GPT partition table bounds");
        goto done;
    }
    /* A table outside the 8 GiB disk reads short and fails its CRC. */
    ssize_t read_bytes = 0;
    table = malloc((size_t)table_size);
    if (!table) {
        image_fail(error, "out of memory reading the GPT partition table");
        goto done;
    }
    if (table_lba < (uint64_t)disk_bytes / SECTOR_BYTES) {
        read_bytes = read_at(fd, table, (size_t)table_size,
                             (off_t)(table_lba * SECTOR_BYTES));
        if (read_bytes < 0) {
            image_fail_errno(error, "cannot read", path);
            goto done;
        }
    }
    if ((uint64_t)read_bytes != table_size ||
        crc32_ieee(0, table, (size_t)table_size) != entries_crc) {
        image_fail(error, "invalid GPT partition table CRC");
        goto done;
    }
    int nonzero = 0;
    for (size_t index = 0; index < (size_t)table_size && !nonzero; index++)
        nonzero = table[index] != 0;
    if (!nonzero) {
        image_fail(error, "empty GPT partition table");
        goto done;
    }
    rc = 0;

done:
    free(table);
    close(fd);
    return rc;
}
