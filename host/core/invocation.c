#include "cli.h"

#include <limits.h>
#include <stdio.h>
#include <string.h>

static char invocation_path[PATH_MAX];

void cli_set_invocation_path(const char *path)
{
    invocation_path[0] = '\0';
    if (!path || !path[0])
        return;
    (void)snprintf(invocation_path, sizeof(invocation_path), "%s", path);
}

const char *cli_invocation_path(void)
{
    return invocation_path[0] ? invocation_path : NULL;
}
