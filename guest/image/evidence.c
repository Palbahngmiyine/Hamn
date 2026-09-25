#include "image/evidence.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "image/digest.h"

#define EVIDENCE_COPY_BYTES (1024 * 1024)

struct output_paths {
    char *parent;      /* resolved parent directory */
    char *name;
    char *targets[2];  /* DESTINATION and DESTINATION.sha256, resolved */
};

static void output_paths_free(struct output_paths *paths)
{
    free(paths->parent);
    free(paths->name);
    free(paths->targets[0]);
    free(paths->targets[1]);
    memset(paths, 0, sizeof(*paths));
}

static char *join_path(const char *directory, const char *name)
{
    size_t directory_length = strlen(directory);
    int separator = directory_length == 0 ||
                    directory[directory_length - 1] != '/';
    size_t length = directory_length + (size_t)separator + strlen(name) + 1;
    char *joined = malloc(length);
    if (joined)
        snprintf(joined, length, "%s%s%s", directory, separator ? "/" : "", name);
    return joined;
}

/*
 * pathlib-style split: empty and "." components are dropped, the name is the
 * last remaining component ("" when none), and the parent is "/" or "." when
 * only the name remains.
 */
static int split_path(const char *path, char **parent, char **name)
{
    size_t length = strlen(path);
    char *parent_text = malloc(length + 2);
    char *name_text = malloc(length + 1);
    if (!parent_text || !name_text) {
        free(parent_text);
        free(name_text);
        return -1;
    }
    int absolute = path[0] == '/';
    size_t parent_length = 0;
    name_text[0] = '\0';
    const char *cursor = path;
    while (*cursor) {
        while (*cursor == '/')
            cursor++;
        const char *start = cursor;
        while (*cursor && *cursor != '/')
            cursor++;
        size_t component = (size_t)(cursor - start);
        if (component == 0 || (component == 1 && start[0] == '.'))
            continue;
        if (name_text[0]) {
            /* The previous name becomes part of the parent. */
            if (parent_length > 0 || absolute)
                parent_text[parent_length++] = '/';
            size_t previous = strlen(name_text);
            memcpy(parent_text + parent_length, name_text, previous);
            parent_length += previous;
        }
        memcpy(name_text, start, component);
        name_text[component] = '\0';
    }
    if (parent_length == 0) {
        strcpy(parent_text, absolute ? "/" : ".");
    } else {
        parent_text[parent_length] = '\0';
    }
    *parent = parent_text;
    *name = name_text;
    return 0;
}

/* Python's non-strict Path.resolve(): resolve the longest existing prefix. */
static char *resolve_lenient(const char *path, unsigned depth)
{
    char *resolved = realpath(path, NULL);
    if (resolved || depth > 4096)
        return resolved;
    char *parent = NULL, *name = NULL;
    if (split_path(path, &parent, &name) != 0)
        return NULL;
    if (!name[0]) {
        free(parent);
        free(name);
        return NULL;
    }
    char *base = resolve_lenient(parent, depth + 1);
    free(parent);
    if (!base) {
        free(name);
        return NULL;
    }
    if (strcmp(name, "..") == 0) {
        char *slash = strrchr(base, '/');
        if (slash && slash != base)
            *slash = '\0';
        else if (slash)
            slash[1] = '\0';
        free(name);
        return base;
    }
    char *joined = join_path(base, name);
    free(base);
    free(name);
    return joined;
}

static int output_paths(const char *destination, char *const reserved[],
                        size_t reserved_count, struct output_paths *paths,
                        struct image_error *error)
{
    memset(paths, 0, sizeof(*paths));
    char *parent = NULL, *name = NULL;
    if (split_path(destination, &parent, &name) != 0)
        return image_fail(error, "out of memory");
    struct stat info;
    if (lstat(parent, &info) != 0) {
        image_fail_errno(error, "cannot inspect baseline output directory",
                         parent);
        free(parent);
        free(name);
        return -1;
    }
    if (!S_ISDIR(info.st_mode) || info.st_uid != geteuid() ||
        (info.st_mode & 0022) != 0) {
        free(parent);
        free(name);
        return image_fail(error, "unsafe baseline output directory");
    }
    int name_valid = name[0] != '\0';
    for (const unsigned char *c = (const unsigned char *)name; *c; c++) {
        if (*c < 32)
            name_valid = 0;
    }
    if (!name_valid) {
        free(parent);
        free(name);
        return image_fail(error, "invalid baseline output name");
    }
    paths->parent = realpath(parent, NULL);
    free(parent);
    paths->name = name;
    if (!paths->parent) {
        image_fail_errno(error, "cannot resolve baseline output directory",
                         destination);
        output_paths_free(paths);
        return -1;
    }
    size_t name_length = strlen(name);
    char *sidecar = malloc(name_length + sizeof(".sha256"));
    if (sidecar)
        snprintf(sidecar, name_length + sizeof(".sha256"), "%s.sha256", name);
    paths->targets[0] = join_path(paths->parent, name);
    paths->targets[1] = sidecar ? join_path(paths->parent, sidecar) : NULL;
    free(sidecar);
    if (!paths->targets[0] || !paths->targets[1]) {
        output_paths_free(paths);
        return image_fail(error, "out of memory");
    }
    for (size_t target = 0; target < 2; target++) {
        int collides = 0;
        if (lstat(paths->targets[target], &info) == 0) {
            collides = 1;
        } else if (errno != ENOENT) {
            image_fail_errno(error, "cannot inspect baseline output",
                             paths->targets[target]);
            output_paths_free(paths);
            return -1;
        }
        for (size_t index = 0; !collides && index < reserved_count; index++) {
            char *reserved_parent = NULL, *reserved_name = NULL;
            if (split_path(reserved[index], &reserved_parent,
                           &reserved_name) != 0) {
                output_paths_free(paths);
                return image_fail(error, "out of memory");
            }
            char *base = resolve_lenient(reserved_parent, 0);
            char *resolved = base ? join_path(base, reserved_name) : NULL;
            free(base);
            free(reserved_parent);
            free(reserved_name);
            if (!resolved) {
                output_paths_free(paths);
                return image_fail(error, "cannot resolve reserved artifact: %s",
                                  reserved[index]);
            }
            collides = strcmp(resolved, paths->targets[target]) == 0;
            free(resolved);
        }
        if (collides) {
            output_paths_free(paths);
            return image_fail(error,
                "baseline output collides with an existing or reserved artifact");
        }
    }
    return 0;
}

int evidence_check(const char *destination, char *const reserved[],
                   size_t reserved_count, struct image_error *error)
{
    struct output_paths paths;
    if (output_paths(destination, reserved, reserved_count, &paths, error) != 0)
        return -1;
    output_paths_free(&paths);
    return 0;
}

static int real_sync(int fd, void *context)
{
    (void)context;
    return fsync(fd);
}

static int real_link(const char *source, int directory_fd, const char *name,
                     void *context)
{
    (void)context;
    /* linkat never replaces NAME: an existing competitor fails with EEXIST. */
    return linkat(AT_FDCWD, source, directory_fd, name, 0);
}

static int write_all(int fd, const void *bytes, size_t length)
{
    const char *cursor = bytes;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        cursor += written;
        length -= (size_t)written;
    }
    return 0;
}

/* Reads REPORT's expected baseline digest and byte count. */
static int report_baseline(const char *report, const struct stat *source,
                           char expected[SHA256_HEX_CAP],
                           struct image_error *error)
{
    struct json_value *value = image_read_json(report, error);
    if (!value)
        return -1;
    const struct json_value *digest = json_object_get(value, "baselineSha256");
    const struct json_value *size =
        json_object_get(value, "baselineCompressedBytes");
    long long bytes = 0;
    int rc = 0;
    if (value->type != JSON_OBJECT)
        rc = image_fail(error, "same-build report is not a JSON object");
    else if (!digest)
        rc = image_fail(error, "same-build report lacks baselineSha256");
    else if (!size)
        rc = image_fail(error, "same-build report lacks baselineCompressedBytes");
    else if (source->st_size <= 0 || json_integer_value(size, &bytes) != 0 ||
             bytes != (long long)source->st_size)
        rc = image_fail(error, "baseline size differs from same-build report");
    else if (!image_json_lower_hex(digest, SHA256_HEX_CAP - 1))
        rc = image_fail(error, "baseline digest differs from same-build report");
    else
        memcpy(expected, digest->text, SHA256_HEX_CAP);
    json_free(value);
    return rc;
}

struct published_link {
    const char *name;
    dev_t device;
    ino_t inode;
};

static void remove_owned_links(int directory_fd, struct published_link *links,
                               size_t count)
{
    while (count > 0) {
        count--;
        struct stat info;
        if (fstatat(directory_fd, links[count].name, &info,
                    AT_SYMLINK_NOFOLLOW) == 0 &&
            info.st_dev == links[count].device &&
            info.st_ino == links[count].inode)
            unlinkat(directory_fd, links[count].name, 0);
    }
}

int evidence_publish(const char *source, const char *destination,
                     const char *report, const struct evidence_io *io,
                     struct image_error *error)
{
    static const struct evidence_io real_io = { real_sync, real_link, NULL };
    if (!io)
        io = &real_io;
    struct output_paths paths;
    if (output_paths(destination, NULL, 0, &paths, error) != 0)
        return -1;
    struct stat source_info, report_info;
    if (!image_owned_single_link(source, &source_info) ||
        !image_owned_single_link(report, &report_info)) {
        output_paths_free(&paths);
        return image_fail(error, "unsafe baseline evidence source");
    }
    char expected[SHA256_HEX_CAP];
    if (report_baseline(report, &source_info, expected, error) != 0) {
        output_paths_free(&paths);
        return -1;
    }

    int rc = -1;
    int directory_fd = -1, source_fd = -1, stage_fd = -1, checksum_fd = -1;
    char *stage_directory = NULL, *stage = NULL, *checksum = NULL;
    char *sidecar_name = NULL;
    unsigned char *buffer = NULL;
    struct published_link links[2];
    size_t published = 0;
    struct stat stage_info, checksum_info;

    directory_fd = open(paths.parent, O_RDONLY | O_DIRECTORY | O_NOFOLLOW |
                                      O_CLOEXEC);
    if (directory_fd < 0) {
        image_fail_errno(error, "cannot open baseline output directory",
                         paths.parent);
        goto done;
    }
    stage_directory = join_path(paths.parent, ".hamn-baseline-XXXXXX");
    if (!stage_directory || !mkdtemp(stage_directory)) {
        if (stage_directory)
            image_fail_errno(error, "cannot create baseline stage",
                             paths.parent);
        else
            image_fail(error, "out of memory");
        free(stage_directory);
        stage_directory = NULL;
        goto done;
    }
    stage = join_path(stage_directory, "image.img");
    checksum = join_path(stage_directory, "image.img.sha256");
    sidecar_name = strrchr(paths.targets[1], '/') + 1;
    buffer = malloc(EVIDENCE_COPY_BYTES);
    if (!stage || !checksum || !buffer) {
        image_fail(error, "out of memory");
        goto done;
    }

    source_fd = open(source, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (source_fd < 0) {
        image_fail_errno(error, "cannot open baseline source", source);
        goto done;
    }
    stage_fd = open(stage, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                    0600);
    if (stage_fd < 0) {
        image_fail_errno(error, "cannot create baseline stage", stage);
        goto done;
    }
    struct stat opened;
    if (fstat(source_fd, &opened) != 0 ||
        opened.st_dev != source_info.st_dev ||
        opened.st_ino != source_info.st_ino || opened.st_nlink != 1 ||
        opened.st_uid != geteuid() || (opened.st_mode & 0022) != 0) {
        image_fail(error, "baseline source changed while opening");
        goto done;
    }
    if (fchmod(stage_fd, 0600) != 0) {
        image_fail_errno(error, "cannot secure baseline stage", stage);
        goto done;
    }
    struct sha256 context;
    sha256_init(&context);
    for (;;) {
        ssize_t count = read(source_fd, buffer, EVIDENCE_COPY_BYTES);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            image_fail_errno(error, "cannot read baseline source", source);
            goto done;
        }
        if (count == 0)
            break;
        sha256_update(&context, buffer, (size_t)count);
        if (write_all(stage_fd, buffer, (size_t)count) != 0) {
            image_fail_errno(error, "cannot write baseline stage", stage);
            goto done;
        }
    }
    if (io->sync(stage_fd, io->context) != 0) {
        image_fail_errno(error, "cannot sync baseline stage", stage);
        goto done;
    }
    if (fstat(stage_fd, &stage_info) != 0) {
        image_fail_errno(error, "cannot inspect baseline stage", stage);
        goto done;
    }
    unsigned char digest[SHA256_DIGEST_BYTES];
    char actual[SHA256_HEX_CAP];
    sha256_final(&context, digest);
    sha256_hex(digest, actual);
    if (stage_info.st_size != source_info.st_size ||
        strcmp(actual, expected) != 0) {
        image_fail(error, "baseline digest differs from same-build report");
        goto done;
    }

    checksum_fd = open(checksum, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW |
                                 O_CLOEXEC, 0600);
    if (checksum_fd < 0) {
        image_fail_errno(error, "cannot create baseline sidecar", checksum);
        goto done;
    }
    char line[SHA256_HEX_CAP + 4];
    snprintf(line, sizeof(line), "%s  ", expected);
    if (fchmod(checksum_fd, 0600) != 0 ||
        write_all(checksum_fd, line, strlen(line)) != 0 ||
        write_all(checksum_fd, paths.name, strlen(paths.name)) != 0 ||
        write_all(checksum_fd, "\n", 1) != 0) {
        image_fail_errno(error, "cannot write baseline sidecar", checksum);
        goto done;
    }
    if (io->sync(checksum_fd, io->context) != 0) {
        image_fail_errno(error, "cannot sync baseline sidecar", checksum);
        goto done;
    }
    if (fstat(checksum_fd, &checksum_info) != 0) {
        image_fail_errno(error, "cannot inspect baseline sidecar", checksum);
        goto done;
    }

    /* The sidecar appears first; the image link completes publication. */
    const char *sources[2] = { checksum, stage };
    const char *names[2] = { sidecar_name, paths.name };
    const struct stat *infos[2] = { &checksum_info, &stage_info };
    for (size_t index = 0; index < 2; index++) {
        if (io->link(sources[index], directory_fd, names[index],
                     io->context) != 0) {
            image_fail_errno(error, "cannot publish baseline evidence",
                             paths.targets[index == 0 ? 1 : 0]);
            goto done;
        }
        links[published].name = names[index];
        links[published].device = infos[index]->st_dev;
        links[published].inode = infos[index]->st_ino;
        published++;
    }
    if (io->sync(directory_fd, io->context) != 0) {
        image_fail_errno(error, "cannot sync baseline output directory",
                         paths.parent);
        goto done;
    }
    rc = 0;

done:
    if (source_fd >= 0)
        close(source_fd);
    if (stage_fd >= 0)
        close(stage_fd);
    if (checksum_fd >= 0)
        close(checksum_fd);
    if (stage)
        unlink(stage);
    if (checksum)
        unlink(checksum);
    /* A stage that cannot be removed fails the publication as a whole. */
    if (stage_directory && rmdir(stage_directory) != 0 && rc == 0)
        rc = image_fail_errno(error, "cannot remove baseline stage",
                              stage_directory);
    if (rc != 0 && directory_fd >= 0)
        remove_owned_links(directory_fd, links, published);
    if (directory_fd >= 0)
        close(directory_fd);
    free(stage_directory);
    free(stage);
    free(checksum);
    free(buffer);
    output_paths_free(&paths);
    return rc;
}
