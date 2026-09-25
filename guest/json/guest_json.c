/*
 * guest-json: the guest image's JSON checks for configuration scripts,
 * installed as /usr/local/libexec/hamn/guest-json.
 *
 *   guest-json docker-daemon CONTAINERD_SOCKET GATEWAY EXTRA_JSON
 *       Merges the profile's docker.daemonJson (EXTRA_JSON, empty for none)
 *       with Hamn-managed daemon keys and writes daemon.json to stdout as
 *       Python's json.dump(indent=2, sort_keys=True) plus a newline did.
 *   guest-json image-manifest PATH
 *       Validates /etc/hamn/guest-image.json against the image contract.
 *
 * Exit status: 0 on success, 1 on rejected input or I/O failure with one
 * "hamn: ..." line on stderr, 2 on a usage error. No other side effects;
 * callers own atomic replacement of any file written from stdout.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "strict_json.h"

#define MANIFEST_MAX_BYTES (64 * 1024)

static const char *const managed_daemon_keys[] = {
    "containerd", "host-gateway-ip", "hosts", "data-root", "exec-root",
    "dns", "bip", "bridge", "fixed-cidr", "default-address-pools",
};

static const char *const manifest_components[] = {
    "docker", "buildkit", "containerd", "runc", "cni", "binfmt", "dnsmasq",
    "hamnd",
};

static int usage(void)
{
    fprintf(stderr,
            "usage: guest-json docker-daemon CONTAINERD_SOCKET GATEWAY EXTRA_JSON\n"
            "       guest-json image-manifest PATH\n");
    return 2;
}

static int write_all(int fd, const char *bytes, size_t length)
{
    while (length > 0) {
        ssize_t written = write(fd, bytes, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        bytes += written;
        length -= (size_t)written;
    }
    return 0;
}

static int docker_fail(const char *message)
{
    fprintf(stderr, "hamn: %s\n", message);
    return 1;
}

static int set_or_fail(struct json_value *object, const char *key,
                       struct json_value *value)
{
    if (!value || json_object_set(object, key, value) != 0) {
        json_free(value);
        return -1;
    }
    return 0;
}

static int docker_daemon(const char *socket, const char *gateway,
                         const char *extra_text)
{
    /* Managed values reach daemon.json verbatim; reject undecodable bytes. */
    if (!json_utf8_valid(socket, strlen(socket)) ||
        !json_utf8_valid(gateway, strlen(gateway)))
        return docker_fail("containerd socket and gateway must be UTF-8");
    struct json_value *extra;
    if (extra_text[0]) {
        struct json_error error;
        extra = json_parse(extra_text, strlen(extra_text), &error);
        if (!extra) {
            fprintf(stderr,
                    "hamn: docker.daemonJson must be one strict JSON object: %s\n",
                    error.message);
            return 1;
        }
    } else {
        extra = json_new_container(JSON_OBJECT);
        if (!extra)
            return docker_fail("cannot allocate Docker daemon settings");
    }
    if (extra->type != JSON_OBJECT) {
        json_free(extra);
        return docker_fail("docker.daemonJson must be one JSON object");
    }
    for (size_t index = 0;
         index < sizeof(managed_daemon_keys) / sizeof(managed_daemon_keys[0]);
         index++) {
        if (json_object_get(extra, managed_daemon_keys[index])) {
            fprintf(stderr,
                    "hamn: docker.daemonJson cannot override Hamn-managed key: %s\n",
                    managed_daemon_keys[index]);
            json_free(extra);
            return 1;
        }
    }
    struct json_value *features = json_object_take(extra, "features");
    if (!features)
        features = json_new_container(JSON_OBJECT);
    if (!features) {
        json_free(extra);
        return docker_fail("cannot allocate Docker daemon settings");
    }
    if (features->type != JSON_OBJECT) {
        json_free(features);
        json_free(extra);
        return docker_fail("docker.daemonJson.features must be a JSON object");
    }
    struct json_value *buildkit = json_object_get(features, "buildkit");
    if (buildkit && buildkit->type != JSON_TRUE) {
        json_free(features);
        json_free(extra);
        return docker_fail("docker.daemonJson.features.buildkit must remain true");
    }
    struct json_value *dns = json_new_container(JSON_ARRAY);
    struct json_value *dns_server = json_new_cstring("172.17.0.1");
    int failed = !dns || !dns_server || json_array_append(dns, dns_server) != 0;
    if (failed) {
        json_free(dns_server);
        json_free(dns);
        json_free(features);
        json_free(extra);
        return docker_fail("cannot allocate Docker daemon settings");
    }
    if (set_or_fail(features, "buildkit", json_new_literal(JSON_TRUE)) != 0) {
        json_free(dns);
        json_free(features);
        json_free(extra);
        return docker_fail("cannot allocate Docker daemon settings");
    }
    /* json_object_set owns FEATURES and DNS only after it succeeds. */
    if (set_or_fail(extra, "containerd", json_new_cstring(socket)) != 0 ||
        json_object_set(extra, "features", features) != 0) {
        json_free(dns);
        json_free(features);
        json_free(extra);
        return docker_fail("cannot allocate Docker daemon settings");
    }
    if (set_or_fail(extra, "bip", json_new_cstring("172.17.0.1/16")) != 0 ||
        json_object_set(extra, "dns", dns) != 0) {
        json_free(dns);
        json_free(extra);
        return docker_fail("cannot allocate Docker daemon settings");
    }
    if (set_or_fail(extra, "host-gateway-ip", json_new_cstring(gateway)) != 0) {
        json_free(extra);
        return docker_fail("cannot allocate Docker daemon settings");
    }
    struct json_buffer output = { 0 };
    int rc = json_dump(extra, 1, &output) == 0 &&
             json_buffer_append(&output, "\n", 1) == 0 ? 0 : -1;
    json_free(extra);
    if (rc != 0) {
        json_buffer_free(&output);
        return docker_fail("cannot serialize Docker daemon settings");
    }
    rc = write_all(STDOUT_FILENO, output.data, output.length);
    json_buffer_free(&output);
    if (rc != 0) {
        fprintf(stderr, "hamn: cannot write Docker daemon settings: %s\n",
                strerror(errno));
        return 1;
    }
    return 0;
}

static int manifest_fail(const char *reason)
{
    fprintf(stderr,
            "hamn: guest image contract: invalid guest image manifest: %s\n",
            reason);
    return 1;
}

static int read_manifest(const char *path, char **text, size_t *length)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0)
        return -1;
    struct stat info;
    if (fstat(fd, &info) != 0) {
        int saved = errno;
        close(fd);
        errno = saved;
        return -1;
    }
    if (!S_ISREG(info.st_mode)) {
        close(fd);
        errno = EINVAL;
        return -1;
    }
    char *buffer = malloc(MANIFEST_MAX_BYTES + 1);
    if (!buffer) {
        close(fd);
        errno = ENOMEM;
        return -1;
    }
    size_t used = 0;
    for (;;) {
        ssize_t count = read(fd, buffer + used, MANIFEST_MAX_BYTES + 1 - used);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            int saved = errno;
            free(buffer);
            close(fd);
            errno = saved;
            return -1;
        }
        if (count == 0)
            break;
        used += (size_t)count;
        if (used > MANIFEST_MAX_BYTES) {
            free(buffer);
            close(fd);
            errno = EFBIG;
            return -1;
        }
    }
    close(fd);
    *text = buffer;
    *length = used;
    return 0;
}

static int manifest_components_valid(const struct json_value *components)
{
    const size_t required =
        sizeof(manifest_components) / sizeof(manifest_components[0]);
    if (!components || components->type != JSON_ARRAY ||
        components->count != required)
        return 0;
    /* Equal counts plus each required name present means the same set. */
    for (size_t name = 0; name < required; name++) {
        int found = 0;
        for (size_t index = 0; index < components->count; index++) {
            const struct json_value *item = components->members[index].value;
            if (item->type != JSON_STRING)
                return 0;
            found |= json_string_equals(item, manifest_components[name]);
        }
        if (!found)
            return 0;
    }
    return 1;
}

static int image_manifest(const char *path)
{
    char *text = NULL;
    size_t length = 0;
    if (read_manifest(path, &text, &length) != 0)
        return manifest_fail(strerror(errno));
    struct json_error error;
    struct json_value *manifest = json_parse(text, length, &error);
    free(text);
    if (!manifest)
        return manifest_fail(error.message);
    int rc = 0;
    long long schema = 0;
    if (manifest->type != JSON_OBJECT || manifest->count != 4 ||
        !json_object_get(manifest, "schemaVersion") ||
        !json_object_get(manifest, "distribution") ||
        !json_object_get(manifest, "architecture") ||
        !json_object_get(manifest, "components"))
        rc = manifest_fail("schema is invalid");
    else if (json_integer_value(json_object_get(manifest, "schemaVersion"),
                                &schema) != 0 || schema != 1)
        rc = manifest_fail("schemaVersion is invalid");
    else if (!json_string_equals(json_object_get(manifest, "distribution"),
                                 "ubuntu-24.04"))
        rc = manifest_fail("distribution is invalid");
    else if (!json_string_equals(json_object_get(manifest, "architecture"),
                                 "arm64"))
        rc = manifest_fail("architecture is invalid");
    else if (!manifest_components_valid(json_object_get(manifest, "components")))
        rc = manifest_fail("component set is invalid");
    json_free(manifest);
    return rc;
}

int main(int argc, char **argv)
{
    if (argc == 5 && strcmp(argv[1], "docker-daemon") == 0)
        return docker_daemon(argv[2], argv[3], argv[4]);
    if (argc == 3 && strcmp(argv[1], "image-manifest") == 0)
        return image_manifest(argv[2]);
    return usage();
}
