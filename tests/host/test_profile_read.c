#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include "core/profile.h"
#include "core/control.h"
#include "core/retirement.h"
#include "util/fs.h"
#include <string.h>
#include "cjson/cJSON.h"

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
    char *json = NULL;
    assert(hamn_control_query(NULL, &json) == 0);
    cJSON *items = cJSON_Parse(json);
    assert(cJSON_IsArray(items) && cJSON_GetArraySize(items) == 0);
    cJSON_Delete(items);
    hamn_control_free(json);
    assert(access(root, F_OK) == -1);
    assert(profile_read_existing(&profile, "../escape") == -1);
    assert(errno == EINVAL);
    assert(profile_load(&profile, "existing") == 0);
    assert(profile_save(&profile) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(hamn_control_start("../escape", 0, 0, 0) == 2);
    assert(hamn_control_configure("existing", 2, 2, 0, 0) == 0);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(profile.cpus == 2 && profile.mem_mib == 2048);
    assert(profile.disk_gib == 60);
    assert(hamn_control_configure("existing", 0, UINT_MAX, 0, 0) == 2);
    assert(profile_read_existing(&profile, "existing") == 0);
    assert(profile.mem_mib == 2048);
    char owner_dir[1024], owner[1100], tombstone[1100], marker[1400];
    snprintf(owner_dir, sizeof(owner_dir), "%s/.kube-contexts", root);
    assert(mkdir(owner_dir, 0700) == 0);
    snprintf(owner, sizeof(owner), "%s/existing", owner_dir);
    snprintf(marker, sizeof(marker), "schema=1\npath=%s/.kube/config\ncontext=foreign\n", temporary);
    assert(fs_write_file_atomic(owner, marker, strlen(marker), 0600) == 0);
    assert(retirement_context(&profile) == -1);
    snprintf(marker, sizeof(marker), "schema=1\npath=%s/.kube/config\ncontext=hamn-existing\n", temporary);
    assert(fs_write_file_atomic(owner, marker, strlen(marker), 0600) == 0);
    assert(retirement_context(&profile) == 0);
    assert(access(owner, F_OK) == -1);
    snprintf(tombstone, sizeof(tombstone), "%s/.retired-kube-contexts/existing", root);
    assert(access(tombstone, F_OK) == 0);
    assert(retirement_context(&profile) == 0);
    assert(unlink(tombstone) == 0);
    assert(rmdir(owner_dir) == 0);
    snprintf(owner_dir, sizeof(owner_dir), "%s/.retired-kube-contexts", root);
    assert(rmdir(owner_dir) == 0);
    assert(hamn_control_query("existing", &json) == 0);
    items = cJSON_Parse(json);
    assert(cJSON_IsObject(items));
    assert(cJSON_IsNumber(cJSON_GetObjectItem(items, "cpus")));
    cJSON_Delete(items);
    hamn_control_free(json);
    char config[1024];
    assert(profile_path(&profile, "config.yaml", config, sizeof(config)));
    assert(unlink(config) == 0);
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(errno == ENOENT);
    assert(rmdir(profile.dir) == 0);
    snprintf(config, sizeof(config), "%s/.existing-mutation.lock", root);
    assert(unlink(config) == 0);
    snprintf(config, sizeof(config), "%s/.locks/existing.lock", root);
    assert(unlink(config) == 0);
    snprintf(config, sizeof(config), "%s/.locks", root);
    assert(rmdir(config) == 0);
    assert(rmdir(root) == 0);
    char foreign[1024];
    snprintf(foreign, sizeof(foreign), "%s/foreign", temporary);
    assert(mkdir(foreign, 0700) == 0);
    assert(symlink(foreign, root) == 0);
    assert(profile_read_existing(&profile, "existing") == -1);
    assert(hamn_control_query(NULL, &json) == -1);
    assert(hamn_control_configure("escape", 2, 2, 0, 1) != 0);
    assert(unlink(root) == 0);
    assert(mkdir(root, 0700) == 0);
    snprintf(config, sizeof(config), "%s/escape", root);
    assert(symlink(foreign, config) == 0);
    assert(profile_load(&profile, "escape") == -1);
    assert(unlink(config) == 0);
    snprintf(config, sizeof(config), "%s/.locks", root);
    assert(symlink(foreign, config) == 0);
    assert(hamn_control_configure("escape", 2, 2, 0, 1) != 0);
    assert(unlink(config) == 0);
    assert(rmdir(root) == 0);
    assert(rmdir(foreign) == 0); /* No state or lock was created in the target. */
    assert(rmdir(temporary) == 0);
    puts("read-only profiles: passed");
    return 0;
}
