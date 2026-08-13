#include <dirent.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "cli.h"
#include "core/log.h"
#include "core/profile.h"
#include "core/state.h"

/* cmd_status.c */
const char *vm_live_state(const struct profile *p, char *buf, size_t cap);

int cmd_list(int argc, char **argv)
{
    (void)argc;
    (void)argv;

    char root[1024];
    if (!hamn_home(root, sizeof(root)))
        die("HOME is not set");

    DIR *d = opendir(root);
    if (!d) {
        logmsg("no profiles yet (run: hamn start)");
        return 0;
    }

    printf("%-12s %-10s %-16s %5s %-10s %-8s %-18s\n", "PROFILE",
           "STATUS", "ADDRESS", "CPUS", "MEMORY", "DISK", "DOCKER CONTEXT");

    struct dirent *e;
    int count = 0;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.' || strcmp(e->d_name, "cache") == 0)
            continue;

        char conf[2048];
        snprintf(conf, sizeof(conf), "%s/%s/config.yaml", root, e->d_name);
        if (access(conf, R_OK) != 0)
            continue;
        char deleted[2048];
        snprintf(deleted, sizeof(deleted), "%s/%s/deleted", root,
                 e->d_name);
        if (access(deleted, F_OK) == 0)
            continue;

        struct profile p;
        if (profile_load(&p, e->d_name) != 0)
            continue;
        struct vm_state st;
        state_load(&p, &st);
        char live[32];
        vm_live_state(&p, live, sizeof(live));

        char mem[32], disk[32];
        snprintf(mem, sizeof(mem), "%uGiB", p.mem_mib / 1024);
        snprintf(disk, sizeof(disk), "%uGiB", p.disk_gib);
        char context[128];
        if (profile_docker_context_name(&p, context, sizeof(context)) != 0)
            continue;
        printf("%-12s %-10s %-16s %5u %-10s %-8s %-18s\n", p.name, live,
               strcmp(live, "running") == 0 && st.ip[0] ? st.ip : "-",
               p.cpus, mem, disk, context);
        count++;
    }
    closedir(d);
    if (count == 0)
        logmsg("no profiles yet (run: hamn start)");
    return 0;
}
