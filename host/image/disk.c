#include "image/disk.h"

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/log.h"
#include "image/qcow2.h"

int disk_prepare(const struct profile *p, const char *cache_img)
{
    char disk[1024];
    profile_path(p, "disk.img", disk, sizeof(disk));
    off_t want = (off_t)p->disk_gib << 30;

    struct stat st;
    if (stat(disk, &st) != 0) {
        logmsg("preparing disk image (%u GiB) ...", p->disk_gib);
        char *err = NULL;
        if (qcow2_extract(cache_img, disk, &err) != 0) {
            logerr("%s", err ? err : "qcow2 extraction failed");
            free(err);
            return -1;
        }
        if (stat(disk, &st) != 0)
            return -1;
    }

    if (st.st_size < want) {
        int fd = open(disk, O_WRONLY);
        if (fd < 0)
            return -1;
        int rc = ftruncate(fd, want);
        close(fd);
        if (rc != 0) {
            logerr("cannot grow disk to %u GiB", p->disk_gib);
            return -1;
        }
    }
    return 0;
}
