#include <mach-o/dyld.h>

#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    char executable[PATH_MAX];
    uint32_t length = sizeof(executable);
    if (_NSGetExecutablePath(executable, &length) != 0)
        return 2;
    char *slash = strrchr(executable, '/');
    if (!slash)
        return 3;
    *slash = '\0';
    char resource[PATH_MAX];
    if (snprintf(resource, sizeof(resource), "%s/../shared-resource",
                 executable) >= (int)sizeof(resource))
        return 4;
    int fd = open(resource, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return 5;
    char content[64];
    ssize_t count = read(fd, content, sizeof(content) - 1);
    close(fd);
    if (count <= 0)
        return 6;
    content[count] = '\0';
    content[strcspn(content, "\r\n")] = '\0';
    printf("approved-mach-o|%s|%s|%s", argc > 1 ? argv[1] : "",
           getenv("PLUGIN_ENV") ? getenv("PLUGIN_ENV") : "", content);
    return 0;
}
