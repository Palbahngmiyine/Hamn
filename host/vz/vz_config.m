#import <Foundation/Foundation.h>
#import <Virtualization/Virtualization.h>

#include <fcntl.h>
#include <unistd.h>

#include "vz/vz_shim.h"

static NSError *mk_err(NSString *msg)
{
    return [NSError errorWithDomain:@"dev.hamn"
                               code:1
                           userInfo:@{NSLocalizedDescriptionKey : msg}];
}

static VZEFIBootLoader *make_bootloader(const vz_vm_spec *spec,
                                        NSError **outErr)
{
    VZEFIBootLoader *efi = [[VZEFIBootLoader alloc] init];
    NSURL *url = [NSURL fileURLWithPath:@(spec->efi_vars)];
    VZEFIVariableStore *vs;

    if ([[NSFileManager defaultManager] fileExistsAtPath:@(spec->efi_vars)]) {
        vs = [[VZEFIVariableStore alloc] initWithURL:url];
    } else {
        vs = [[VZEFIVariableStore alloc]
            initCreatingVariableStoreAtURL:url
                                   options:0
                                     error:outErr];
        if (!vs)
            return nil;
    }
    efi.variableStore = vs;
    return efi;
}

static VZGenericPlatformConfiguration *make_platform(const vz_vm_spec *spec,
                                                     NSError **outErr)
{
    VZGenericPlatformConfiguration *platform =
        [[VZGenericPlatformConfiguration alloc] init];

    if (spec->nested_virtualization) {
        if (@available(macOS 15.0, *)) {
            if (![VZGenericPlatformConfiguration
                    isNestedVirtualizationSupported]) {
                *outErr = mk_err(@"nested virtualization requires an Apple silicon Mac with M3 or later");
                return nil;
            }
            platform.nestedVirtualizationEnabled = YES;
        } else {
            *outErr = mk_err(@"nested virtualization requires macOS 15 or later");
            return nil;
        }
    }

    if (!spec->machine_id_file)
        return platform; /* 비영속 식별자 (테스트 부팅용) */

    NSString *path = @(spec->machine_id_file);
    if ([[NSFileManager defaultManager] fileExistsAtPath:path]) {
        NSData *data = [NSData dataWithContentsOfFile:path];
        VZGenericMachineIdentifier *mid = [[VZGenericMachineIdentifier alloc]
            initWithDataRepresentation:data];
        if (!mid) {
            *outErr = mk_err(@"corrupt machine identifier file");
            return nil;
        }
        platform.machineIdentifier = mid;
    } else {
        VZGenericMachineIdentifier *mid =
            [[VZGenericMachineIdentifier alloc] init];
        if (![mid.dataRepresentation writeToFile:path atomically:YES]) {
            *outErr = mk_err(@"cannot persist machine identifier");
            return nil;
        }
        platform.machineIdentifier = mid;
    }
    return platform;
}

static VZVirtioBlockDeviceConfiguration *make_disk(const char *path,
                                                   BOOL readOnly,
                                                   NSError **outErr)
{
    VZDiskImageStorageDeviceAttachment *att =
        [[VZDiskImageStorageDeviceAttachment alloc]
            initWithURL:[NSURL fileURLWithPath:@(path)]
               readOnly:readOnly
                  error:outErr];
    if (!att)
        return nil;
    return [[VZVirtioBlockDeviceConfiguration alloc] initWithAttachment:att];
}

VZVirtualMachineConfiguration *hamn_vz_build_config(const vz_vm_spec *spec,
                                                    NSError **outErr)
{
    VZVirtualMachineConfiguration *cfg =
        [[VZVirtualMachineConfiguration alloc] init];
    cfg.CPUCount = spec->cpus;
    cfg.memorySize = spec->mem_bytes;

    cfg.bootLoader = make_bootloader(spec, outErr);
    if (!cfg.bootLoader)
        return nil;

    VZGenericPlatformConfiguration *platform = make_platform(spec, outErr);
    if (!platform)
        return nil;
    cfg.platform = platform;

    /* 디스크: 메인(rw) + seed ISO(ro, 선택) */
    NSMutableArray *disks = [NSMutableArray array];
    VZVirtioBlockDeviceConfiguration *main_disk =
        make_disk(spec->disk_img, NO, outErr);
    if (!main_disk)
        return nil;
    [disks addObject:main_disk];
    if (spec->seed_iso) {
        VZVirtioBlockDeviceConfiguration *seed =
            make_disk(spec->seed_iso, YES, outErr);
        if (!seed)
            return nil;
        [disks addObject:seed];
    }
    cfg.storageDevices = disks;

    /* Every Hamn VM uses Virtualization.framework shared NAT. */
    VZVirtioNetworkDeviceConfiguration *net =
        [[VZVirtioNetworkDeviceConfiguration alloc] init];
    net.attachment = [[VZNATNetworkDeviceAttachment alloc] init];
    if (spec->mac_addr) {
        VZMACAddress *mac =
            [[VZMACAddress alloc] initWithString:@(spec->mac_addr)];
        if (!mac) {
            *outErr = mk_err(@"invalid MAC address");
            return nil;
        }
        net.MACAddress = mac;
    }
    cfg.networkDevices = @[ net ];

    /* 시리얼 콘솔 → 로그 파일 (게스트 hvc0) */
    if (spec->serial_log) {
        int fd = open(spec->serial_log, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            *outErr = mk_err(@"cannot open serial log file");
            return nil;
        }
        NSFileHandle *wh =
            [[NSFileHandle alloc] initWithFileDescriptor:fd
                                          closeOnDealloc:YES];
        VZFileHandleSerialPortAttachment *spa =
            [[VZFileHandleSerialPortAttachment alloc]
                initWithFileHandleForReading:nil
                        fileHandleForWriting:wh];
        VZVirtioConsoleDeviceSerialPortConfiguration *serial =
            [[VZVirtioConsoleDeviceSerialPortConfiguration alloc] init];
        serial.attachment = spa;
        cfg.serialPorts = @[ serial ];
    }

    /* virtiofs 공유 */
    NSMutableArray *sharing = [NSMutableArray array];
    for (int i = 0; i < spec->nshares; i++) {
        VZVirtioFileSystemDeviceConfiguration *fsd =
            [[VZVirtioFileSystemDeviceConfiguration alloc]
                initWithTag:@(spec->shares[i].tag)];
        VZSharedDirectory *dir = [[VZSharedDirectory alloc]
            initWithURL:[NSURL fileURLWithPath:@(spec->shares[i].host_path)]
               readOnly:spec->shares[i].read_only ? YES : NO];
        fsd.share = [[VZSingleDirectoryShare alloc] initWithDirectory:dir];
        [sharing addObject:fsd];
    }
    if (spec->rosetta) {
        NSError *rosetta_error = nil;
        VZLinuxRosettaDirectoryShare *rosetta =
            [[VZLinuxRosettaDirectoryShare alloc] initWithError:&rosetta_error];
        if (!rosetta) {
            *outErr = rosetta_error ?: mk_err(@"Linux Intel binary translation is unavailable");
            return nil;
        }
        VZVirtioFileSystemDeviceConfiguration *fsd =
            [[VZVirtioFileSystemDeviceConfiguration alloc]
                initWithTag:@"rosetta"];
        fsd.share = rosetta;
        [sharing addObject:fsd];
    }
    cfg.directorySharingDevices = sharing;

    cfg.entropyDevices =
        @[ [[VZVirtioEntropyDeviceConfiguration alloc] init] ];
    cfg.memoryBalloonDevices =
        @[ [[VZVirtioTraditionalMemoryBalloonDeviceConfiguration alloc] init] ];

    return cfg;
}
