#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include "core/profile.h"

int main(void)
{
    char temporary[] = "/tmp/hamn-profile-read-XXXXXX";
    assert(mkdtemp(temporary));
    assert(setenv("HOME", temporary, 1) == 0);
    struct profile profile;
    assert(profile_read_existing(&profile, "missing") == -1);
    assert(errno == ENOENT);
    char root[1024];
    assert(hamn_home(root, sizeof(root)));
    assert(access(root, F_OK) == -1);
    assert(profile_read_existing(&profile, "../escape") == -1);
    assert(errno == EINVAL);
    assert(profile_load(&profile, "existing") == 0);
    assert(profile_save(&profile) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    char config[1024];
    assert(profile_path(&profile, "config.yaml", config, sizeof(config)));
    assert(unlink(config) == 0);
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(errno == ENOENT);
    assert(rmdir(profile.dir) == 0);
    assert(rmdir(root) == 0);
    assert(rmdir(temporary) == 0);
    puts("read-only profiles: passed");
    return 0;
}
