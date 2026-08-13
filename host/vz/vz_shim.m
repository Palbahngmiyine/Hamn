#import <Foundation/Foundation.h>
#import <Virtualization/Virtualization.h>

#include <stdlib.h>

#include "vz/vz_shim.h"

/* vz_config.m */
VZVirtualMachineConfiguration *hamn_vz_build_config(const vz_vm_spec *spec,
                                                    NSError **outErr);

static void err_from_ns(char **err, NSError *e, const char *what)
{
    if (!err)
        return;
    const char *msg = e ? e.localizedDescription.UTF8String : "unknown error";
    if (asprintf(err, "%s: %s", what, msg) < 0)
        *err = NULL;
}

@interface CimaVM : NSObject <VZVirtualMachineDelegate>
@property (strong) VZVirtualMachine *vm;
@property (strong) dispatch_queue_t queue;
@property (assign) vz_state_cb cb;
@property (assign) void *ud;
@end

@implementation CimaVM

- (void)guestDidStopVirtualMachine:(VZVirtualMachine *)virtualMachine
{
    (void)virtualMachine;
    if (self.cb)
        self.cb(self.ud, VZ_ST_STOPPED);
}

- (void)virtualMachine:(VZVirtualMachine *)virtualMachine
      didStopWithError:(NSError *)error
{
    (void)virtualMachine;
    fprintf(stderr, "hamn: vm stopped with error: %s\n",
            error.localizedDescription.UTF8String);
    if (self.cb)
        self.cb(self.ud, VZ_ST_ERROR);
}

@end

struct vz_vm {
    void *obj; /* CFBridgingRetain(CimaVM *) */
};

static CimaVM *handle(vz_vm *v)
{
    return (__bridge CimaVM *)v->obj;
}

vz_vm *vz_vm_create(const vz_vm_spec *spec, vz_state_cb cb, void *ud,
                    char **err)
{
    @autoreleasepool {
        NSError *e = nil;
        VZVirtualMachineConfiguration *cfg = hamn_vz_build_config(spec, &e);
        if (!cfg) {
            err_from_ns(err, e, "vz: build configuration");
            return NULL;
        }
        if (![cfg validateWithError:&e]) {
            err_from_ns(err, e, "vz: validate configuration");
            return NULL;
        }

        CimaVM *h = [CimaVM new];
        h.queue = dispatch_queue_create("dev.hamn.vm", DISPATCH_QUEUE_SERIAL);
        h.cb = cb;
        h.ud = ud;

        dispatch_sync(h.queue, ^{
            h.vm = [[VZVirtualMachine alloc] initWithConfiguration:cfg
                                                             queue:h.queue];
            h.vm.delegate = h;
        });

        vz_vm *out = calloc(1, sizeof(*out));
        out->obj = (void *)CFBridgingRetain(h);
        return out;
    }
}

int vz_vm_start(vz_vm *v, char **err)
{
    CimaVM *h = handle(v);
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    __block NSError *se = nil;

    dispatch_async(h.queue, ^{
        [h.vm startWithCompletionHandler:^(NSError *e) {
            se = e;
            dispatch_semaphore_signal(sem);
        }];
    });
    dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);

    if (se) {
        err_from_ns(err, se, "vz: start");
        return -1;
    }
    return 0;
}

int vz_vm_request_stop(vz_vm *v, char **err)
{
    CimaVM *h = handle(v);
    __block NSError *e = nil;
    __block BOOL ok = NO;

    dispatch_sync(h.queue, ^{
        ok = [h.vm requestStopWithError:&e];
    });
    if (!ok) {
        err_from_ns(err, e, "vz: request stop");
        return -1;
    }
    return 0;
}

int vz_vm_force_stop(vz_vm *v, char **err)
{
    CimaVM *h = handle(v);
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    __block NSError *se = nil;

    dispatch_async(h.queue, ^{
        [h.vm stopWithCompletionHandler:^(NSError *e) {
            se = e;
            dispatch_semaphore_signal(sem);
        }];
    });
    dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);

    if (se) {
        err_from_ns(err, se, "vz: force stop");
        return -1;
    }
    return 0;
}

enum vz_state vz_vm_state(vz_vm *v)
{
    CimaVM *h = handle(v);
    __block VZVirtualMachineState st;

    dispatch_sync(h.queue, ^{
        st = h.vm.state;
    });
    switch (st) {
    case VZVirtualMachineStateStopped:
        return VZ_ST_STOPPED;
    case VZVirtualMachineStateRunning:
        return VZ_ST_RUNNING;
    case VZVirtualMachineStatePaused:
        return VZ_ST_PAUSED;
    case VZVirtualMachineStateError:
        return VZ_ST_ERROR;
    case VZVirtualMachineStateStarting:
        return VZ_ST_STARTING;
    case VZVirtualMachineStateStopping:
        return VZ_ST_STOPPING;
    default:
        return VZ_ST_OTHER;
    }
}

void vz_runloop(void)
{
    dispatch_main();
}
