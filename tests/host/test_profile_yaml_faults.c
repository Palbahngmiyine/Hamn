/* The profile readers when libyaml runs out of memory. The Makefile compiles
 * vendor/libyaml/src/api.c into this test with malloc and realloc renamed to
 * the two functions below, which fail on request and otherwise call the real
 * ones. Every allocation of the parser goes through that file.
 *
 * Out of memory is not a rule that a file breaks: the reader must report it
 * as ENOMEM for each allocation that can fail, and the list of profiles must
 * then fail as a whole instead of listing a profile whose file is valid as
 * one that cannot be read. */
#undef malloc
#undef realloc

#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "core/control.h"
#include "core/log.h"
#include "core/profile.h"
#include "util/fs.h"

static long allocations;        /* calls of the allocator since the reset */
static long failing = -1;       /* the call that fails, counted from 0; -1: none */

static int fails_now(void)
{
    return allocations++ == failing;
}

void *fault_malloc(size_t size)
{
    if (fails_now()) {
        errno = ENOMEM;
        return NULL;
    }
    return malloc(size);
}

void *fault_realloc(void *pointer, size_t size)
{
    if (fails_now()) {
        errno = ENOMEM;
        return NULL;
    }
    return realloc(pointer, size);
}

static void fail_allocation(long index)
{
    allocations = 0;
    failing = index;
}

#define VALID "cpus: 2\nmemoryMiB: 2048\nmounts:\n" \
    "  - location: /data\n    mountPoint: /workspace\n"
#define DEFINITION "apiVersion: hamn/v1\nkind: Profile\nmetadata:\n" \
    "  name: valid\nspec:\n  cpus: 2\n"
#define EXHAUSTED "cannot read the configuration of profile valid: " \
    "Cannot allocate memory"

int main(void)
{
    char home[] = "/tmp/hamn-yaml-faults-XXXXXX";
    assert(mkdtemp(home));
    assert(setenv("HOME", home, 1) == 0);
    struct profile profile;
    char reason[PROFILE_REASON_CAP], path[1024], directory[1024], root[1024];
    assert(profile_load(&profile, "valid") == 0);
    assert(profile_path(&profile, "config.yaml", path, sizeof(path)));
    snprintf(directory, sizeof(directory), "%s", profile.dir);
    assert(hamn_home(root, sizeof(root)));
    assert(fs_write_file_atomic(path, VALID, strlen(VALID), 0600) == 0);

    /* How many allocations a read of this file makes when none fails. */
    fail_allocation(-1);
    assert(profile_read_existing_reason(&profile, "valid", reason) == 0);
    assert(profile.cpus == 2 && profile.mount_count == 1 && !reason[0]);
    long reads = allocations;
    assert(reads > 4);

    /* Each of them failing, the first as the last, is out of memory: never
     * a reason that blames the file, and never a profile that was read. */
    for (long index = 0; index < reads; index++) {
        fail_allocation(index);
        memset(reason, 'x', sizeof(reason));
        errno = 0;
        int rc = profile_read_existing_reason(&profile, "valid", reason);
        if (rc != -1 || errno != ENOMEM ||
            strcmp(reason, strerror(ENOMEM)) != 0) {
            fprintf(stderr, "allocation %ld of %ld: rc %d errno %d reason "
                    "\"%s\"\n", index, reads, rc, errno, reason);
            abort();
        }
    }

    /* The definition of `vm apply` is read by the same parser. */
    fail_allocation(-1);
    assert(profile_definition_parse(DEFINITION, strlen(DEFINITION), &profile,
                                    reason) == 0);
    long definitions = allocations;
    for (long index = 0; index < definitions; index++) {
        fail_allocation(index);
        errno = 0;
        assert(profile_definition_parse(DEFINITION, strlen(DEFINITION),
                                        &profile, reason) == -1);
        assert(errno == ENOMEM && strcmp(reason, strerror(ENOMEM)) == 0);
    }

    /* The list fails as a whole and names the profile; it does not list a
     * profile whose file is valid as one that cannot be read. So does the
     * status. The last allocation of the parse is one that only the parse of
     * the document makes, not the creation of the parser. */
    char *json = (char *)"";
    fail_allocation(reads - 1);
    assert(hamn_control_query(NULL, &json) == -1 && json == NULL);
    assert(strcmp(log_last_error(), EXHAUSTED) == 0);
    json = (char *)"";
    fail_allocation(reads - 1);
    assert(hamn_control_query("valid", &json) == -1 && json == NULL);
    assert(strcmp(log_last_error(), EXHAUSTED) == 0);

    /* Without a failure both answer. */
    fail_allocation(-1);
    assert(hamn_control_query(NULL, &json) == 0 && strstr(json, "\"cpus\":2"));
    assert(!strstr(json, "configurationError"));
    hamn_control_free(json);

    assert(unlink(path) == 0 && rmdir(directory) == 0 && rmdir(root) == 0);
    assert(rmdir(home) == 0);
    puts("profile readers out of memory: passed");
    return 0;
}
