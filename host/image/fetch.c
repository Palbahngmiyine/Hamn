#include "image/fetch.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "cjson/cJSON.h"
#include "core/log.h"
#include "core/profile.h"
#include "util/fs.h"
#define MANAGED_IMAGE_CONFIG "guest-image.json"
#define UPDATE_TRANSACTION ".hamn-update-transaction"

static int hex_sha256_valid(const char *value)
{
    if (!value || strlen(value) != 64)
        return 0;
    for (const char *cursor = value; *cursor; cursor++) {
        if (!(*cursor >= '0' && *cursor <= '9') &&
            !(*cursor >= 'a' && *cursor <= 'f'))
            return 0;
    }
    return 1;
}

static int read_small_owned_file(const char *path, char *output, size_t cap)
{
    if (cap < 2)
        return -1;
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return -1;
    struct stat status;
    if (fstat(fd, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_uid != geteuid() || status.st_nlink != 1 ||
        status.st_size < 0 || (unsigned long long)status.st_size >= cap) {
        (void)close(fd);
        errno = EINVAL;
        return -1;
    }
    size_t offset = 0;
    while (offset < (size_t)status.st_size) {
        ssize_t count = read(fd, output + offset,
                             (size_t)status.st_size - offset);
        if (count < 0 && errno == EINTR)
            continue;
        if (count <= 0) {
            (void)close(fd);
            return -1;
        }
        offset += (size_t)count;
    }
    if (close(fd) != 0)
        return -1;
    output[offset] = '\0';
    return 0;
}

static int managed_image_name_valid(const char *name, const char *hash)
{
    static const char prefix[] = "hamn-guest-";
    size_t prefix_length = sizeof(prefix) - 1;
    return name && hash && strncmp(name, prefix, prefix_length) == 0 &&
        strlen(name) == prefix_length + 64 + 4 &&
        strcmp(name + prefix_length + 64, ".img") == 0 &&
        strncmp(name + prefix_length, hash, 64) == 0;
}

/* The updater records the old binary and selection before either public
 * pointer changes.  A pending record means a hard interruption can still be
 * rolled back by the next signed update, so do not let start use either half
 * of that transaction. */
static int update_transaction_pending(const char *cache)
{
    char transaction[1024];
    int written = snprintf(transaction, sizeof(transaction), "%s/%s", cache,
                           UPDATE_TRANSACTION);
    if (written < 0 || written >= (int)sizeof(transaction)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    struct stat status;
    if (lstat(transaction, &status) == 0) {
        errno = EBUSY;
        return 1;
    }
    return errno == ENOENT ? 0 : -1;
}

/* A signed update selects an already hash-verified guest image through this
 * private cache manifest. Invalid selection data is never downgraded to an
 * unsigned cloud-image fetch. */
static int managed_image_ensure(const char *cache, char *img_out, size_t cap)
{
    char config[1024], text[4096];
    int written = snprintf(config, sizeof(config), "%s/%s", cache,
                           MANAGED_IMAGE_CONFIG);
    if (written < 0 || written >= (int)sizeof(config)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    if (access(config, F_OK) != 0)
        return errno == ENOENT ? 0 : -1;
    if (read_small_owned_file(config, text, sizeof(text)) != 0)
        return -1;
    cJSON *json = cJSON_ParseWithOpts(text, NULL, 1);
    cJSON *schema = json ? cJSON_GetObjectItemCaseSensitive(json,
                                                              "schemaVersion") : NULL;
    cJSON *file = json ? cJSON_GetObjectItemCaseSensitive(json, "file") : NULL;
    cJSON *hash = json ? cJSON_GetObjectItemCaseSensitive(json, "sha256") : NULL;
    int key_count = 0;
    for (cJSON *child = json ? json->child : NULL; child; child = child->next)
        key_count++;
    int valid = cJSON_IsObject(json) && key_count == 3 &&
        cJSON_IsNumber(schema) && schema->valuedouble == 1.0 &&
        cJSON_IsString(file) && cJSON_IsString(hash) &&
        hex_sha256_valid(hash->valuestring) &&
        managed_image_name_valid(file->valuestring, hash->valuestring);
    if (!valid) {
        cJSON_Delete(json);
        errno = EINVAL;
        return -1;
    }
    char image[1024], marker[1100], marker_text[80];
    written = snprintf(image, sizeof(image), "%s/%s", cache,
                       file->valuestring);
    if (written < 0 || written >= (int)sizeof(image) ||
        snprintf(marker, sizeof(marker), "%s.verified", image) >=
            (int)sizeof(marker) ||
        read_small_owned_file(marker, marker_text, sizeof(marker_text)) != 0) {
        cJSON_Delete(json);
        errno = EINVAL;
        return -1;
    }
    int marker_valid = strcmp(marker_text, hash->valuestring) == 0 ||
        (strlen(marker_text) == 65 && marker_text[64] == '\n' &&
         strncmp(marker_text, hash->valuestring, 64) == 0);
    if (!marker_valid) {
        cJSON_Delete(json);
        errno = EINVAL;
        return -1;
    }
    struct stat status;
    if (lstat(image, &status) != 0 || !S_ISREG(status.st_mode) ||
        status.st_uid != geteuid() || status.st_nlink != 1 ||
        snprintf(img_out, cap, "%s", image) >= (int)cap) {
        cJSON_Delete(json);
        errno = EINVAL;
        return -1;
    }
    cJSON_Delete(json);
    return 1;
}

int fetch_image_ensure(char *img_out, size_t cap)
{
    char cache[1024];
    if (!hamn_home(cache, sizeof(cache)))
        return -1;
    strlcat(cache, "/cache", sizeof(cache));
    if (fs_mkdirs(cache, 0755) != 0)
        return -1;

    int pending = update_transaction_pending(cache);
    if (pending != 0) {
        logerr(pending > 0 ?
               "an interrupted update recovery is pending; run hamn update before starting a VM" :
               "cannot inspect the signed update recovery state");
        return -1;
    }

    int managed = managed_image_ensure(cache, img_out, cap);
    if (managed > 0)
        return 0;
    if (managed < 0) {
        logerr("managed guest image selection is invalid; run a signed update "
               "again or remove only the invalid cache selection");
        return -1;
    }

    /* ENOENT is a recoverable first-start condition.  The lifecycle command
     * invokes the managed signed updater and retries; other failures above
     * remain fail-closed. */
    errno = ENOENT;
    return -1;
}
