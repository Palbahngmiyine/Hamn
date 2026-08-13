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

static int k3s_ready(const struct profile *profile, const struct vm_state *state,
                     const char *live)
{
    if (!profile->kubernetes_enabled || strcmp(live, "running") != 0 ||
        !state->ip[0])
        return 0;
    const char *command[] = {
        "sudo", "systemctl", "is-active", "--quiet", "k3s.service", NULL
    };
    return ssh_exec(profile, state->ip, command, 1) == 0;
}

int cmd_status(int argc, char **argv)
{
    int json_output = 0;
    const char *flag_profile = NULL;
    static const struct option options[] = {
        { "profile", required_argument, NULL, 'p' },
        { "output", required_argument, NULL, 'o' },
        { "json", no_argument, NULL, 'j' },
        { 0 },
    };
    optind = 1;
    optreset = 1;
    int option;
    while ((option = getopt_long(argc, argv, "p:o:j", options, NULL)) != -1) {
        if (option == 'p')
            flag_profile = optarg;
        else if (option == 'o' && strcmp(optarg, "json") == 0)
            json_output = 1;
        else if (option == 'j')
            json_output = 1;
        else {
            fprintf(stderr,
                    "usage: hamn status [-p PROFILE] [PROFILE] [--json]\n");
            return 2;
        }
    }
    if (optind + 1 < argc) {
        fprintf(stderr,
                "usage: hamn status [-p PROFILE] [PROFILE] [--json]\n");
        return 2;
    }

    char profile_name[PROFILE_NAME_CAP];
    if (profile_resolve_name(flag_profile,
                             optind < argc ? argv[optind] : NULL,
                             profile_name) != 0) {
        logerr("invalid profile name");
        return 2;
    }

    struct profile p;
    if (profile_load(&p, profile_name) != 0)
        die("cannot load profile");

    struct vm_state st;
    state_load(&p, &st);

    char live[32];
    vm_live_state(&p, live, sizeof(live));
    struct guest_status guest = {0};
    if (strcmp(live, "running") == 0)
        (void)guest_status_read(&p, &guest);
    int kubernetes_ready = k3s_ready(&p, &st, live);

    if (json_output) {
        char context[128];
        cJSON *root = cJSON_CreateObject();
        cJSON *docker = NULL, *cri = NULL, *kubernetes = NULL;
        char docker_socket[PROFILE_PATH_CAP];
        if (profile_docker_context_name(&p, context, sizeof(context)) != 0 ||
            !profile_path(&p, "docker.sock", docker_socket,
                          sizeof(docker_socket)) ||
            !root || !cJSON_AddNumberToObject(root, "schemaVersion", 2) ||
            !cJSON_AddStringToObject(root, "profile", p.name) ||
            !cJSON_AddStringToObject(root, "state", live) ||
            !cJSON_AddNumberToObject(root, "cpus", p.cpus) ||
            !cJSON_AddNumberToObject(root, "memoryMiB", p.mem_mib) ||
            !cJSON_AddNumberToObject(root, "diskGiB", p.disk_gib) ||
            !cJSON_AddStringToObject(root, "dockerContext", context) ||
            !(docker = cJSON_AddObjectToObject(root, "docker")) ||
            !cJSON_AddStringToObject(docker, "socket", docker_socket) ||
            !cJSON_AddBoolToObject(docker, "apiReady",
                                   guest.docker_api_ready) ||
            !(cri = cJSON_AddObjectToObject(root, "cri")) ||
            !cJSON_AddBoolToObject(cri, "ready", guest.cri_ready) ||
            !cJSON_AddStringToObject(cri, "namespace", "k8s.io") ||
            !(kubernetes = cJSON_AddObjectToObject(root, "kubernetes")) ||
            !cJSON_AddBoolToObject(kubernetes, "enabled",
                                   p.kubernetes_enabled) ||
            !cJSON_AddBoolToObject(kubernetes, "ready", kubernetes_ready) ||
            !cJSON_AddStringToObject(root, "directory", p.dir)) {
            cJSON_Delete(root);
            die("cannot encode status JSON");
        }
        if (strcmp(live, "running") == 0 && st.ip[0]) {
            if (!cJSON_AddStringToObject(root, "ip", st.ip)) {
                cJSON_Delete(root);
                die("cannot encode status JSON");
            }
        } else if (!cJSON_AddNullToObject(root, "ip")) {
            cJSON_Delete(root);
            die("cannot encode status JSON");
        }
        char *text = cJSON_PrintUnformatted(root);
        cJSON_Delete(root);
        if (!text)
            die("cannot encode status JSON");
        printf("%s\n", text);
        cJSON_free(text);
    } else {
        printf("profile: %s\n", p.name);
        printf("state:   %s\n", live);
        if (strcmp(live, "running") == 0 && st.ip[0])
            printf("ip:      %s\n", st.ip);
        printf("cpus:    %u\n", p.cpus);
        printf("memory:  %u MiB\n", p.mem_mib);
        printf("disk:    %u GiB\n", p.disk_gib);
        char context[128];
        if (profile_docker_context_name(&p, context, sizeof(context)) != 0)
            die("cannot resolve Docker context");
        printf("docker:  %s\n", context);
        printf("docker API: %s\n", guest.docker_api_ready ? "ready" :
               "not ready");
        printf("CRI:     %s\n", guest.cri_ready ? "ready" : "not ready");
        printf("K3s:     %s\n", p.kubernetes_enabled ?
               (kubernetes_ready ? "ready" : "not ready") : "disabled");
        printf("dir:     %s\n", p.dir);
    }
    return 0;
}
