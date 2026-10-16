/* The Virtualization.framework configuration that vz_config.m builds for a
 * VM (host/vz/vz_config.m): its disk attachments read back through the
 * framework's own properties. The main disk is writable, host-cached and
 * fully synchronized; the seed ISO is read-only. The guest console is
 * attached only for a descriptor the spec names: a zero-initialized spec has
 * none. No VM is started, so this runs without the virtualization
 * entitlement. */
#import <Foundation/Foundation.h>
#import <Virtualization/Virtualization.h>

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "vz/vz_shim.h"

/* Defined in vz_config.m; vz_shim.m declares it the same way. */
VZVirtualMachineConfiguration *hamn_vz_build_config(const vz_vm_spec *spec,
                                                    NSError **outErr);

static void fail(const char *format, ...) __attribute__((format(printf, 1, 2), noreturn));

static void fail(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    fputs("FAIL: ", stderr);
    vfprintf(stderr, format, arguments);
    fputc('\n', stderr);
    va_end(arguments);
    exit(1);
}

#define REQUIRE(condition, ...) \
    do { \
        if (!(condition)) \
            fail(__VA_ARGS__); \
    } while (0)

static void make_file(const char *path, off_t size)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    REQUIRE(fd >= 0, "create %s", path);
    REQUIRE(ftruncate(fd, size) == 0, "size %s", path);
    REQUIRE(close(fd) == 0, "close %s", path);
}

static VZDiskImageStorageDeviceAttachment *disk_at(VZVirtualMachineConfiguration *cfg,
                                                   NSUInteger index)
{
    REQUIRE(index < cfg.storageDevices.count, "no storage device %lu",
            (unsigned long)index);
    VZStorageDeviceConfiguration *device = cfg.storageDevices[index];
    REQUIRE([device isKindOfClass:[VZVirtioBlockDeviceConfiguration class]],
            "storage device %lu is not virtio-blk", (unsigned long)index);
    REQUIRE([device.attachment isKindOfClass:[VZDiskImageStorageDeviceAttachment class]],
            "storage device %lu is not a disk image", (unsigned long)index);
    return (VZDiskImageStorageDeviceAttachment *)device.attachment;
}

static void main_disk_is_cached_and_fully_synchronized(const char *dir)
{
    char disk[PATH_MAX], seed[PATH_MAX], efi[PATH_MAX];
    snprintf(disk, sizeof(disk), "%s/disk.img", dir);
    snprintf(seed, sizeof(seed), "%s/seed.iso", dir);
    snprintf(efi, sizeof(efi), "%s/efi-vars.bin", dir);
    make_file(disk, 64 << 20);
    make_file(seed, 1 << 20);

    vz_vm_spec spec;
    memset(&spec, 0, sizeof(spec));
    spec.cpus = 1;
    spec.mem_bytes = 512ull << 20;
    spec.disk_img = disk;
    spec.seed_iso = seed;
    spec.efi_vars = efi;

    NSError *error = nil;
    VZVirtualMachineConfiguration *cfg = hamn_vz_build_config(&spec, &error);
    REQUIRE(cfg != nil, "build config: %s",
            error.localizedDescription.UTF8String ?: "no error");
    REQUIRE(cfg.storageDevices.count == 2, "expected main disk and seed, got %lu",
            (unsigned long)cfg.storageDevices.count);

    VZDiskImageStorageDeviceAttachment *main_disk = disk_at(cfg, 0);
    REQUIRE([main_disk.URL.path isEqualToString:@(disk)], "main disk is %s",
            main_disk.URL.path.UTF8String);
    REQUIRE(!main_disk.readOnly, "main disk is read-only");
    REQUIRE(main_disk.cachingMode == VZDiskImageCachingModeCached,
            "main disk caching mode %ld, expected Cached (%ld)",
            (long)main_disk.cachingMode, (long)VZDiskImageCachingModeCached);
    REQUIRE(main_disk.synchronizationMode == VZDiskImageSynchronizationModeFull,
            "main disk synchronization mode %ld, expected Full (%ld)",
            (long)main_disk.synchronizationMode,
            (long)VZDiskImageSynchronizationModeFull);

    VZDiskImageStorageDeviceAttachment *seed_disk = disk_at(cfg, 1);
    REQUIRE([seed_disk.URL.path isEqualToString:@(seed)], "seed is %s",
            seed_disk.URL.path.UTF8String);
    REQUIRE(seed_disk.readOnly, "seed ISO is writable");
}

/* serial_fd 0 is "no console": a zeroed spec must not attach stdin. A pipe's
 * write end becomes the one virtio console, through a duplicate. */
static void serial_console_is_attached_only_for_a_descriptor(const char *dir)
{
    char disk[PATH_MAX], efi[PATH_MAX];
    snprintf(disk, sizeof(disk), "%s/disk.img", dir);
    snprintf(efi, sizeof(efi), "%s/efi-vars.bin", dir);
    make_file(disk, 64 << 20);

    vz_vm_spec spec;
    memset(&spec, 0, sizeof(spec));
    spec.cpus = 1;
    spec.mem_bytes = 512ull << 20;
    spec.disk_img = disk;
    spec.efi_vars = efi;
    NSError *error = nil;
    VZVirtualMachineConfiguration *cfg = hamn_vz_build_config(&spec, &error);
    REQUIRE(cfg != nil, "build config: %s",
            error.localizedDescription.UTF8String ?: "no error");
    REQUIRE(cfg.serialPorts.count == 0,
            "a zeroed spec attached %lu serial ports",
            (unsigned long)cfg.serialPorts.count);

    int pipe_fds[2];
    REQUIRE(pipe(pipe_fds) == 0, "pipe: %s", strerror(errno));
    spec.serial_fd = pipe_fds[1];
    cfg = hamn_vz_build_config(&spec, &error);
    REQUIRE(cfg != nil, "build config with a console: %s",
            error.localizedDescription.UTF8String ?: "no error");
    REQUIRE(cfg.serialPorts.count == 1, "expected one serial port, got %lu",
            (unsigned long)cfg.serialPorts.count);
    VZSerialPortConfiguration *port = cfg.serialPorts[0];
    REQUIRE([port isKindOfClass:[VZVirtioConsoleDeviceSerialPortConfiguration class]],
            "the console is not a virtio console");
    REQUIRE([port.attachment isKindOfClass:[VZFileHandleSerialPortAttachment class]],
            "the console is not attached to a file handle");
    VZFileHandleSerialPortAttachment *attachment =
        (VZFileHandleSerialPortAttachment *)port.attachment;
    int attached = attachment.fileHandleForWriting.fileDescriptor;
    REQUIRE(attached != pipe_fds[1] && attached > STDERR_FILENO,
            "the console uses descriptor %d, not a duplicate of %d", attached,
            pipe_fds[1]);
    /* The caller keeps its own descriptor. */
    REQUIRE(close(pipe_fds[1]) == 0 && close(pipe_fds[0]) == 0,
            "the caller's console descriptors must stay open");
}

static char test_dir[] = "/tmp/hamn-vz-config-XXXXXX";

/* Runs on every exit, failures included; a file the test never created is
 * not an error. */
static void remove_dir(void)
{
    const char *names[] = {"disk.img", "seed.iso", "efi-vars.bin"};
    for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
        char path[PATH_MAX];
        snprintf(path, sizeof(path), "%s/%s", test_dir, names[i]);
        if (unlink(path) != 0 && errno != ENOENT)
            fprintf(stderr, "cannot remove %s: %s\n", path, strerror(errno));
    }
    if (rmdir(test_dir) != 0)
        fprintf(stderr, "cannot remove %s: %s\n", test_dir, strerror(errno));
}

int main(void)
{
    @autoreleasepool {
        REQUIRE(mkdtemp(test_dir) != NULL, "mkdtemp");
        REQUIRE(atexit(remove_dir) == 0, "atexit");
        main_disk_is_cached_and_fully_synchronized(test_dir);
        serial_console_is_attached_only_for_a_descriptor(test_dir);
    }
    puts("vz disk config: ok");
    return 0;
}
