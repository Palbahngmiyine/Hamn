#include "util/dhcp_leases.h"

#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define LEASES_FILE "/var/db/dhcpd_leases"

/* "52:54:0:ab:1:2" 또는 "1,52:54:00:ab:01:02" → 6옥텟. 0=성공 */
static int parse_mac(const char *s, unsigned char out[6])
{
    const char *comma = strchr(s, ',');
    if (comma)
        s = comma + 1;
    unsigned v[6];
    if (sscanf(s, "%x:%x:%x:%x:%x:%x", &v[0], &v[1], &v[2], &v[3], &v[4],
               &v[5]) != 6)
        return -1;
    for (int i = 0; i < 6; i++) {
        if (v[i] > 0xff)
            return -1;
        out[i] = (unsigned char)v[i];
    }
    return 0;
}

int dhcp_lookup_ip(const char *mac, char *ip_out, size_t cap)
{
    unsigned char want[6];
    if (parse_mac(mac, want) != 0)
        return -1;

    FILE *f = fopen(LEASES_FILE, "r");
    if (!f)
        return -1;

    /* 블록 단위 파싱: 마지막으로 매치한 항목이 최신 lease */
    char line[512];
    char cur_ip[64] = "";
    int cur_match = 0;
    int found = 0;

    while (fgets(line, sizeof(line), f)) {
        char *p = line;
        while (*p == ' ' || *p == '\t')
            p++;

        if (*p == '{') {
            cur_ip[0] = '\0';
            cur_match = 0;
        } else if (strncmp(p, "ip_address=", 11) == 0) {
            char *v = p + 11;
            v[strcspn(v, "\r\n")] = '\0';
            snprintf(cur_ip, sizeof(cur_ip), "%s", v);
        } else if (strncmp(p, "hw_address=", 11) == 0) {
            unsigned char got[6];
            if (parse_mac(p + 11, got) == 0 &&
                memcmp(got, want, 6) == 0)
                cur_match = 1;
        } else if (*p == '}') {
            if (cur_match && cur_ip[0]) {
                snprintf(ip_out, cap, "%s", cur_ip);
                found = 1; /* 계속 읽어 더 최신 항목 우선 */
            }
        }
    }
    fclose(f);
    return found ? 0 : -1;
}

int dhcp_wait_ip(const char *mac, int timeout_sec, char *ip_out, size_t cap)
{
    for (int i = 0; i < timeout_sec; i++) {
        if (dhcp_lookup_ip(mac, ip_out, cap) == 0)
            return 0;
        sleep(1);
    }
    return dhcp_lookup_ip(mac, ip_out, cap);
}
