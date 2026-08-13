#include <stdio.h>
#include <stdlib.h>

#include "cli.h"
#include "core/log.h"
#include "image/qcow2.h"

int cmd_qcow2_extract(int argc, char **argv)
{
    if (argc != 3) {
        fprintf(stderr, "usage: hamn qcow2-extract <in.qcow2> <out.raw>\n");
        return 1;
    }
    char *err = NULL;
    if (qcow2_extract(argv[1], argv[2], &err) != 0) {
        logerr("%s", err ? err : "qcow2 extraction failed");
        free(err);
        return 1;
    }
    return 0;
}
