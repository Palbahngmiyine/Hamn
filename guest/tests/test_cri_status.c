#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "api/cri_status.h"

int main(int argc, char **argv)
{
    if (argc != 4) {
        fprintf(stderr, "usage: %s CTR_PATH TIMEOUT_MS true|false\n", argv[0]);
        return 2;
    }
    char *end = NULL;
    unsigned long timeout = strtoul(argv[2], &end, 10);
    if (!*argv[1] || !end || *end || timeout == 0 || timeout > 1000 ||
        (strcmp(argv[3], "true") != 0 && strcmp(argv[3], "false") != 0))
        return 2;
    int expected = strcmp(argv[3], "true") == 0;
    int actual = cri_plugin_ready_for_test(argv[1], (unsigned)timeout);
    if (actual != expected) {
        fprintf(stderr, "expected cri readiness %s, got %s\n", argv[3],
                actual ? "true" : "false");
        return 1;
    }
    return 0;
}
