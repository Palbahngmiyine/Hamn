#ifndef HAMN_VZ_SHIM_H
#define HAMN_VZ_SHIM_H

#include <stdint.h>

/*
 * Virtualization.framework에 대한 순수 C 경계.
 * 이 헤더 뒤의 구현(vz_shim.m, vz_config.m)만 Objective-C이며,
 * 나머지 호스트 코드는 전부 .c로 유지한다.
 */

#define VZ_MAX_SHARES 18

typedef struct vz_vm vz_vm;

typedef struct {
    unsigned cpus;
    uint64_t mem_bytes;
    const char *disk_img;        /* raw 디스크 (필수, rw) */
    const char *seed_iso;        /* cloud-init seed (선택, ro) */
    const char *efi_vars;        /* EFI variable store 경로 (필수, 없으면 생성) */
    const char *machine_id_file; /* VZGenericMachineIdentifier 영속 파일 (선택) */
    const char *mac_addr;        /* "52:54:00:xx:xx:xx" (선택, NULL=랜덤) */
    const char *serial_log;      /* 시리얼 콘솔 출력 파일 (선택) */
    int rosetta;                 /* Linux Intel binary translation share */
    int nested_virtualization;   /* supported Apple Silicon hosts only */
    struct {
        const char *tag;         /* virtiofs 태그 */
        const char *host_path;   /* 공유할 호스트 디렉토리 */
        int read_only;
    } shares[VZ_MAX_SHARES];
    int nshares;
} vz_vm_spec;

enum vz_state {
    VZ_ST_STOPPED = 0,
    VZ_ST_RUNNING,
    VZ_ST_PAUSED,
    VZ_ST_ERROR,
    VZ_ST_STARTING,
    VZ_ST_STOPPING,
    VZ_ST_OTHER,
};

/* 상태 콜백은 VM 내부 직렬 큐에서 호출된다. */
typedef void (*vz_state_cb)(void *ud, enum vz_state st);

/* 실패 시 NULL 반환, *err에 malloc된 메시지 (호출자가 free). */
vz_vm *vz_vm_create(const vz_vm_spec *spec, vz_state_cb cb, void *ud,
                    char **err);

/* 동기 시작: 부팅 시작 성공/실패가 결정될 때까지 블록. 0=성공. */
int vz_vm_start(vz_vm *vm, char **err);

/* 게스트에 graceful 종료 요청 (게스트 OS가 처리). 0=요청 성공. */
int vz_vm_request_stop(vz_vm *vm, char **err);

/* 강제 종료 (전원 차단에 해당). 동기. 0=성공. */
int vz_vm_force_stop(vz_vm *vm, char **err);

enum vz_state vz_vm_state(vz_vm *vm);

/* 메인 스레드를 dispatch 런루프로 넘긴다. 반환하지 않는다. */
void vz_runloop(void) __attribute__((noreturn));

#endif
