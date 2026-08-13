#ifndef HAMN_DHCP_LEASES_H
#define HAMN_DHCP_LEASES_H

#include <stddef.h>

/*
 * macOS 내장 DHCP 서버(bootpd, VZ NAT가 사용)의 lease 파일
 * /var/db/dhcpd_leases에서 MAC 주소로 게스트 IP를 찾는다.
 * 주의: 파일의 hw_address는 옥텟 선행 0이 생략된다 (52:54:0:ab:1:2).
 */

/* 0=찾음(ip_out 채움), -1=없음 */
int dhcp_lookup_ip(const char *mac, char *ip_out, size_t cap);

/* timeout_sec 동안 1초 간격 폴링. 0=찾음, -1=타임아웃 */
int dhcp_wait_ip(const char *mac, int timeout_sec, char *ip_out, size_t cap);

#endif
