#include <CommonCrypto/CommonDigest.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "image/fetch.h"

static int write_text(const char *path, const char *text)
{
    FILE *file = fopen(path, "w");
    if (!file)
        return -1;
    int rc = fputs(text, file) >= 0 && fclose(file) == 0 ? 0 : -1;
    if (rc == 0)
        (void)chmod(path, 0600);
    return rc;
}

static void sha256_text(const char *text, char output[65])
{
    unsigned char digest[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(text, (CC_LONG)strlen(text), digest);
    for (size_t index = 0; index < sizeof(digest); index++)
        snprintf(output + index * 2, 3, "%02x", digest[index]);
}

int main(void)
{
    char home[] = "/tmp/hamn-managed-guest.XXXXXX";
    char hamn[PATH_MAX], cache[PATH_MAX], image[PATH_MAX], marker[PATH_MAX];
    char config[PATH_MAX], transaction[PATH_MAX], output[PATH_MAX], hash[65],
        text[512];
    if (!mkdtemp(home) || setenv("HOME", home, 1) != 0 ||
        snprintf(hamn, sizeof(hamn), "%s/.hamn", home) >= (int)sizeof(hamn) ||
        snprintf(cache, sizeof(cache), "%s/cache", hamn) >= (int)sizeof(cache) ||
        mkdir(hamn, 0700) != 0 || mkdir(cache, 0755) != 0)
        return 1;

    if (fetch_image_ensure(output, sizeof(output)) == 0) {
        fprintf(stderr, "FAIL: unsigned stock cloud image fallback was accepted\n");
        return 1;
    }

    sha256_text("signed guest image", hash);
    if (snprintf(image, sizeof(image), "%s/hamn-guest-%s.img", cache, hash) >=
            (int)sizeof(image) ||
        snprintf(marker, sizeof(marker), "%s.verified", image) >=
            (int)sizeof(marker) ||
        snprintf(config, sizeof(config), "%s/guest-image.json", cache) >=
            (int)sizeof(config) ||
        snprintf(transaction, sizeof(transaction), "%s/.hamn-update-transaction",
                 cache) >= (int)sizeof(transaction) ||
        write_text(image, "signed guest image") != 0 ||
        snprintf(text, sizeof(text), "%s\n", hash) >= (int)sizeof(text) ||
        write_text(marker, text) != 0 ||
        snprintf(text, sizeof(text),
                 "{\"schemaVersion\":1,\"file\":\"hamn-guest-%s.img\",\"sha256\":\"%s\"}",
                 hash, hash) >= (int)sizeof(text) ||
        write_text(config, text) != 0 || fetch_image_ensure(output,
                                                              sizeof(output)) != 0 ||
        strcmp(output, image) != 0) {
        fprintf(stderr, "FAIL: signed managed guest image was not selected\n");
        return 1;
    }

    if (mkdir(transaction, 0700) != 0 ||
        fetch_image_ensure(output, sizeof(output)) == 0) {
        fprintf(stderr, "FAIL: pending update transaction did not block VM start\n");
        return 1;
    }
    if (rmdir(transaction) != 0) {
        fprintf(stderr, "FAIL: cannot remove test update transaction\n");
        return 1;
    }

    if (write_text(config,
                   "{\"schemaVersion\":1,\"file\":\"../evil.img\",\"sha256\":\"00\"}") != 0 ||
        fetch_image_ensure(output, sizeof(output)) == 0) {
        fprintf(stderr, "FAIL: invalid managed guest image selection was accepted\n");
        return 1;
    }

    (void)unlink(config);
    (void)unlink(marker);
    (void)unlink(image);
    (void)rmdir(cache);
    (void)rmdir(hamn);
    (void)rmdir(home);
    puts("PASS: signed managed guest image selection is fail-closed");
    return 0;
}
