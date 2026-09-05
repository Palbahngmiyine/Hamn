#include <stdio.h>
#include <string.h>
#include "core/guest_deployment.h"

int main(int argc, char **argv)
{
    struct profile profile = {0};
    if (argc != 2 || strlen(argv[1]) >= sizeof(profile.dir))
        return 2;
    snprintf(profile.dir, sizeof(profile.dir), "%s", argv[1]);
    return guest_deployment_docker_ready(&profile) ? 0 : 1;
}
