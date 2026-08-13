#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "core/log.h"
#include "sshmgr/ssh.h"
#include "util/proc.h"

int ssh_keys_ensure(const struct profile *p)
{
    char key[1024];
    profile_path(p, "id_ed25519", key, sizeof(key));
    if (access(key, R_OK) == 0)
        return 0;

    logmsg("generating ssh key ...");
    const char *argv[] = { "ssh-keygen", "-q", "-t", "ed25519", "-N", "",
                           "-C", "hamn", "-f", key, NULL };
    if (proc_run(argv) != 0) {
        logerr("ssh-keygen failed");
        return -1;
    }
    return 0;
}

int ssh_read_pubkey(const struct profile *p, char *buf, size_t cap)
{
    char pub[1024];
    profile_path(p, "id_ed25519.pub", pub, sizeof(pub));
    FILE *f = fopen(pub, "r");
    if (!f)
        return -1;
    if (!fgets(buf, (int)cap, f)) {
        fclose(f);
        return -1;
    }
    fclose(f);
    buf[strcspn(buf, "\r\n")] = '\0';
    return 0;
}
