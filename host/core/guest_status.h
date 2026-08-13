#ifndef HAMN_GUEST_STATUS_H
#define HAMN_GUEST_STATUS_H

#include "core/profile.h"

/* Read-only status from the Hamn guest agent; this never exposes CRI to macOS. */
struct guest_status {
    int available;
    int docker_api_ready;
    int cri_ready;
};

int guest_status_read(const struct profile *profile, struct guest_status *status);

#endif
