#include <getopt.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cli.h"
#include "cjson/cJSON.h"
#include "core/guest_status.h"
#include "core/lifecycle.h"
#include "core/log.h"
#include "core/profile.h"
#include "core/state.h"
#include "sshmgr/ssh.h"
#include "vmrun/ctlsock.h"

/* 실제 상태: ctl + 프로세스 시작 토큰이 일치해야 신뢰한다. */
const char *vm_live_state(const struct profile *p, char *buf, size_t cap)
{
    char ctl[1024], resp[256];
    profile_path(p, "vmrun.sock", ctl, sizeof(ctl));
    enum vm_process_state process_state = vm_process_probe(p, NULL);
    if (process_state == VM_PROCESS_VERIFIED &&
        ctlsock_query(ctl, "{\"cmd\":\"status\"}", resp, sizeof(resp),
                      300) == 0) {
        const char *k = strstr(resp, "\"state\":\"");
        if (k) {
            k += 9;
            size_t n = strcspn(k, "\"");
            if (n >= cap)
                n = cap - 1;
            memcpy(buf, k, n);
            buf[n] = '\0';
            return buf;
        }
    }
    snprintf(buf, cap, "%s", process_state == VM_PROCESS_STALE ?
             "stopped" : "unknown");
    return buf;
}
