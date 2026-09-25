BUILD      := build
HOST_BIN   := $(BUILD)/hamn
CARGO_PROFILE ?= release
# x-release-please-start-version
VERSION    ?= 0.1.2
# x-release-please-end
VERSION_STAMP := $(BUILD)/.hamn-version
# tools/hamn-dev publishes build/hamn and runs the host regression suites.
HAMN_DEV := $(or $(CARGO_TARGET_DIR),target)/$(if $(filter dev,$(CARGO_PROFILE)),debug,$(CARGO_PROFILE))/hamn-dev
PREFIX     ?= $(HOME)/.local
BINDIR     ?= $(PREFIX)/bin
DATADIR    ?= $(PREFIX)/share/hamn/src

MACOS_MIN  := 13.0
CFLAGS     := -std=c11 -Wall -Wextra -O2 -g \
              -Werror=implicit-function-declaration \
              -MMD -MP \
              -mmacosx-version-min=$(MACOS_MIN) \
              -DHAVE_CONFIG_H \
              -Ihost -Ivendor -Ivendor/libyaml/include
LDFLAGS    := -framework Virtualization -framework Foundation -framework CoreServices -lz \
              -mmacosx-version-min=$(MACOS_MIN)
ifneq ($(strip $(SDKROOT)),)
CFLAGS     += -isysroot $(SDKROOT)
LDFLAGS    += -isysroot $(SDKROOT)
endif
OBJCFLAGS  := $(filter-out -std=c11,$(CFLAGS)) -fobjc-arc

HOST_C_SRCS := $(wildcard host/*.c host/cmd/*.c host/core/*.c host/image/*.c \
                          host/seed/*.c host/sshmgr/*.c host/fwd/*.c \
                          host/vmrun/*.c host/util/*.c) \
               vendor/cjson/cJSON.c $(wildcard vendor/libyaml/src/*.c)
HOST_M_SRCS := $(wildcard host/vz/*.m)
HOST_OBJS   := $(patsubst %.c,$(BUILD)/%.o,$(HOST_C_SRCS)) \
               $(patsubst %.m,$(BUILD)/%.o,$(HOST_M_SRCS))
HOST_DEPS   := $(HOST_OBJS:.o=.d)
HOST_TEST_OBJS := $(filter-out $(BUILD)/host/main.o,$(HOST_OBJS))
# Only these objects compile in the release version, so a version change
# rebuilds them and relinks instead of rebuilding the whole C core.
VERSION_CFLAGS := -DHAMN_VERSION=\"$(VERSION)\"
VERSIONED_OBJS := $(BUILD)/host/cmd/cmd_update.o $(BUILD)/host/cmd/cmd_diagnostics.o \
	$(BUILD)/host/core/control.o
$(VERSIONED_OBJS): CFLAGS += $(VERSION_CFLAGS)
LIFECYCLE_LOCK_TEST := $(BUILD)/tests/test_lifecycle_lock
CTLSOCK_TEST := $(BUILD)/tests/test_ctlsock
FS_TEST := $(BUILD)/tests/test_fs
SEED_MOUNTS_TEST := $(BUILD)/tests/test_cloudinit_mounts
PROVISION_TEST := $(BUILD)/tests/test_provision
DEPLOYMENT_FINGERPRINT_TEST := $(BUILD)/tests/test_guest_deployment_fingerprint
DEPLOYMENT_RECOVERY_TEST := $(BUILD)/tests/test_deployment_recovery
MANAGED_GUEST_IMAGE_TEST := $(BUILD)/tests/test_managed_guest_image
SSH_OPTIONS_TEST := $(BUILD)/tests/test_ssh_options
PROFILE_READ_TEST := $(BUILD)/tests/test_profile_read
START_DOCKER_CONTEXT_RETRY_TEST := $(BUILD)/tests/test_start_docker_context_retry
RAW_CACHE_TEST := $(BUILD)/tests/test_raw_cache
# HAMN_TEST_SANITIZERS=1 builds the raw cache test with ASan and UBSan.
SANITIZER_CFLAGS := -fsanitize=address,undefined -fno-omit-frame-pointer
RAW_CACHE_TEST_CFLAGS := -std=c11 -Wall -Wextra -Werror=implicit-function-declaration \
	-Wno-deprecated-declarations -DHAMN_TEST -Ihost \
	$(if $(filter 1,$(HAMN_TEST_SANITIZERS)),$(SANITIZER_CFLAGS))

-include $(HOST_DEPS)

.PHONY: FORCE host hamn-dev install clean test-portable test-qcow2 test-control \
	test-profile-state test-guest-deployment test-diagnostics test-install \
	test-uninstall test-update test-release-artifacts test-hosted-validation \
	test-release-gate test-release-publish \
	test-release-version \
	test-release-request \
	test-kubernetes-cli test-core-quality test-public-export test-release-repository-preflight \
	test-port-forwarding test-workflows test-local-macos \
	$(test-control_PARTS) $(test-install_PARTS) $(test-update_PARTS) \
	check-ci-macos-shards print-ci-macos-shards test-control-tui \
	test-update-ux-redirected test-update-ux-pty \
	release-candidate release-gate release-hosted-validation

FORCE:

host: $(HOST_BIN)

$(VERSION_STAMP): FORCE
	@mkdir -p $(dir $@)
	@if ! test -f $@ || ! test "$$(cat $@)" = "$(VERSION)"; then \
		printf '%s\n' "$(VERSION)" > $@.tmp; \
		mv $@.tmp $@; \
	fi

$(VERSIONED_OBJS): $(VERSION_STAMP)

install: host
	bash scripts/install-host.sh "$(HOST_BIN)" "$(BINDIR)" "$(DATADIR)"

$(BUILD)/libhamn_core.a: $(HOST_OBJS)
	@mkdir -p $(dir $@)
	rm -f $@.tmp
	ar rcs $@.tmp $(HOST_OBJS)
	mv $@.tmp $@

# The same deployment target as the product build, so both builds reuse one
# set of dependency artifacts.
hamn-dev:
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MIN) cargo build --locked --profile $(CARGO_PROFILE) -p hamn-dev

$(HOST_BIN): FORCE host/entitlements.plist hamn-dev
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MIN) $(HAMN_DEV) build-host $@ $(VERSION) $(CARGO_PROFILE)

$(BUILD)/%.o: %.c
	@mkdir -p $(dir $@)
	clang $(CFLAGS) -c $< -o $@

$(BUILD)/%.o: %.m
	@mkdir -p $(dir $@)
	clang $(OBJCFLAGS) -c $< -o $@

$(LIFECYCLE_LOCK_TEST): tests/host/test_lifecycle_lock.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(CTLSOCK_TEST): tests/host/test_ctlsock.c host/vmrun/ctlsock.c \
                host/vmrun/ctlsock.h
	@mkdir -p $(dir $@)
	clang -DHAMN_TEST $(filter-out -MMD -MP,$(CFLAGS)) $< \
		host/vmrun/ctlsock.c -framework Foundation -o $@

$(FS_TEST): tests/host/test_fs.c host/util/fs.c host/util/fs.h
	@mkdir -p $(dir $@)
	clang -DHAMN_TEST $(filter-out -MMD -MP,$(CFLAGS)) $< host/util/fs.c -o $@

$(SEED_MOUNTS_TEST): tests/host/test_cloudinit_mounts.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(PROVISION_TEST): tests/host/test_provision.c host/core/provision.c \
	host/core/provision.h host/core/profile.h host/sshmgr/ssh.h host/util/fs.c
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< host/core/provision.c \
		host/util/fs.c -o $@

$(DEPLOYMENT_RECOVERY_TEST): tests/host/test_deployment_recovery.c \
		host/core/deployment_recovery.h
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< -o $@

$(DEPLOYMENT_FINGERPRINT_TEST): tests/host/test_guest_deployment_fingerprint.c \
		$(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

# The test substitutes its own extractor for raw_cache.c's qcow2_extract_fd.
$(RAW_CACHE_TEST): tests/host/test_raw_cache.c host/image/raw_cache.c \
		host/image/disk.c host/image/qcow2.c FORCE
	@mkdir -p $(dir $@)
	clang $(RAW_CACHE_TEST_CFLAGS) -Dqcow2_extract_fd=test_extract \
		-c host/image/raw_cache.c -o $@-raw_cache.o
	clang $(RAW_CACHE_TEST_CFLAGS) tests/host/test_raw_cache.c host/image/disk.c \
		host/image/qcow2.c $@-raw_cache.o -lz -o $@

$(MANAGED_GUEST_IMAGE_TEST): tests/host/test_managed_guest_image.c \
		$(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(SSH_OPTIONS_TEST): tests/host/test_ssh_options.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

# Compiles every host source itself, including VERSIONED_OBJS' sources.
$(START_DOCKER_CONTEXT_RETRY_TEST): tests/host/test_start_docker_context_retry.c \
		host/cmd/cmd_start.c $(HOST_C_SRCS) $(VERSION_STAMP)
	@mkdir -p $(dir $@)
	clang -DHAMN_TEST $(filter-out -MMD -MP,$(CFLAGS)) $(VERSION_CFLAGS) $< \
		$(filter-out host/main.c host/cmd/cmd_start.c,$(HOST_C_SRCS)) \
		host/cmd/cmd_start.c $(HOST_M_SRCS) $(LDFLAGS) -o $@

test-portable: hamn-dev
	$(HAMN_DEV) test repository
	bash guest/tests/test_configure_containerd.sh
	bash guest/tests/test_configure_docker.sh
	bash guest/tests/test_configure_rosetta.sh
	bash guest/tests/test_make_install_targets.sh
	bash guest/tests/test_guest_deployment_transaction.sh
	bash guest/tests/test_verify_image_contract.sh
	bash guest/tests/test_guest_image_builder.sh

$(PROFILE_READ_TEST): tests/host/test_profile_read.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) $(LDFLAGS) -o $@

$(BUILD)/tests/test_docker_readiness: tests/host/test_docker_readiness.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) $(LDFLAGS) -o $@

$(BUILD)/tests/test_proc_deadline: tests/host/test_proc_deadline.c host/util/proc.c host/util/proc.h
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< host/util/proc.c -o $@

$(BUILD)/tests/test_ssh_deadline: tests/host/test_ssh_deadline.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) $(LDFLAGS) -o $@

# These share one debug test build: the native-flag-inventory and tui suites run
# `cargo test` binaries themselves, so they stay with `cargo test --locked`.
test-control-rust: host
	$(HAMN_DEV) test native-flag-inventory
	cargo test --locked
	@test "$$(cargo tree --locked --prefix none --format '{p}' | sed -n '/^crossterm v/p' | cut -d ' ' -f 1,2 | sort -u | wc -l | tr -d ' ')" = 1
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui

test-control-native: host $(PROFILE_READ_TEST) $(BUILD)/tests/test_docker_readiness $(BUILD)/tests/test_proc_deadline $(BUILD)/tests/test_ssh_deadline
	$(HAMN_DEV) test control-signed-bootstrap
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-create-plugins
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_operation.c host/core/operation.c host/core/log.c host/util/fs.c vendor/cjson/cJSON.c -o $(BUILD)/tests/test_operation
	$(BUILD)/tests/test_operation
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_operation_preflight.c host/core/operation.c host/core/log.c host/util/fs.c vendor/cjson/cJSON.c -o $(BUILD)/tests/test_operation_preflight
	$(BUILD)/tests/test_operation_preflight
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_remote_mutation.c host/core/remote_mutation.c -o $(BUILD)/tests/test_remote_mutation
	$(BUILD)/tests/test_remote_mutation
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_proc_early_exit.c -o $(BUILD)/tests/test_proc_early_exit
	$(BUILD)/tests/test_proc_early_exit
	$(HAMN_DEV) test remote-cancel-boundaries
	python3 tests/host/workspace_live_transport.py
	HAMN=$(HOST_BIN) $(HAMN_DEV) test start-preflight
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_proc_cancellation.c host/util/proc.c -o $(BUILD)/tests/test_proc_cancellation
	$(BUILD)/tests/test_proc_cancellation
	$(BUILD)/tests/test_proc_deadline
	SSH_DEADLINE_TEST=$(BUILD)/tests/test_ssh_deadline $(HAMN_DEV) test ssh-deadline
	$(HAMN_DEV) test docker-readiness
	$(PROFILE_READ_TEST)
	HAMN=$(HOST_BIN) $(HAMN_DEV) test core-worker
	HAMN=$(HOST_BIN) $(HAMN_DEV) test docker-api
	HAMN=$(HOST_BIN) $(HAMN_DEV) test docker-context
	HAMN=$(HOST_BIN) $(HAMN_DEV) test kubernetes-api
	HAMN=$(HOST_BIN) $(HAMN_DEV) test exec-auth

# The TUI, workspace and packaging regressions of test-control, after test-control-native.
test-control-tui: host
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-navigation
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-review-improvements
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-session-management
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-workspaces
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-outcomes
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-native-regressions
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-quoting
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-reload
	HAMN=$(HOST_BIN) $(HAMN_DEV) test native-query-lifetime
	python3 tests/host/test_workspace_live_prepare.py
	HAMN=$(HOST_BIN) python3 tests/host/test_workspace_management.py
	python3 tests/host/test_workspace_live_external_contexts.py
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-docker-all
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-docker-images
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-docker-config
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-kubectl-output
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-cluster-target
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-picker-restore
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-environment-actions
	python3 tests/host/test_workspace_kubernetes_assertions.py
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-tls-target
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-backpressure
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-guarded-delete
	HAMN=$(HOST_BIN) $(HAMN_DEV) test tui-ssh-timeout
	cargo test --locked -p hamn-dev
	$(HAMN_DEV) test rust-sdk
	HAMN=$(HOST_BIN) $(HAMN_DEV) test single-binary

test-workflows:
	@command -v actionlint >/dev/null || { \
		echo "actionlint is required for test-workflows" >&2; exit 1; \
	}
	actionlint -config-file .github/actionlint.yaml .github/workflows/*.yml

# Every local macOS gate, in order. test-local-macos runs them serially in one
# checkout (the release workflow does this before publishing). Gates listed
# with <gate>_PARTS are run as those parts, in order.
LOCAL_MACOS_GATES := test-workflows test-portable test-core-quality test-control \
	test-port-forwarding host test-profile-state test-guest-deployment \
	test-diagnostics test-install test-uninstall test-update test-kubernetes-cli \
	test-release-artifacts test-release-gate test-hosted-validation \
	test-release-publish test-release-version test-release-request \
	test-public-export test-release-repository-preflight
test-control_PARTS := test-control-native test-control-tui test-control-rust
test-install_PARTS := test-install-script test-install-cleanup \
	test-install-system-tools test-install-bootstrap
test-update_PARTS := test-update-properties test-update-native \
	test-update-concurrency test-update-recovery-legacy test-update-recovery-current \
	test-update-check test-update-cli test-update-ux-redirected test-update-ux-pty \
	test-update-script
LOCAL_MACOS_LEAVES := $(foreach gate,$(LOCAL_MACOS_GATES),$(or $($(gate)_PARTS),$(gate)))

# PR CI runs the same leaves on independent macOS machines (GitHub runs at
# most five macOS jobs at once), each shard in its own checkout. Installer
# and updater gates of one user must not overlap: generation cleanup refuses
# to prune while any process of that user has scripts/update-host.sh open
# without the transaction lock, and a non-root lsof sees only its own
# user's processes. Each shard runs, after building build/hamn once:
# - CI_MACOS_SHARD_<n>: the main lane, serially, except its
#   CI_MACOS_SIDE_GATES, which run beside it. Side gates never run the
#   updater, never open scripts/update-host.sh, never rebuild build/hamn and
#   only read it;
# - CI_MACOS_SHARD_<n>_USER: a second updater lane of CI_MACOS_USER_GATES,
#   which only read the checkout. With CI_MACOS_LANE_UID set (CI creates that
#   user) it runs beside the main lane as that user, without rebuilding
#   build/hamn; otherwise it runs after the main lane;
# - CI_MACOS_ALONE_GATES last, with nothing beside them: they rebuild
#   build/hamn as other versions, or (test-control-rust) hold cancellation
#   deadline tests that timed out beside other lanes (run 36140673204).
# Balance by measured durations; check-ci-macos-shards proves the shards
# partition the leaves and every lane holds only gates of its class.
CI_MACOS_SHARDS := 1 2 3 4 5
# Timing-sensitive PTY gates (test-control-native, test-control-tui) run
# beside at most one heavy updater lane. test-control-rust stays on shard 4,
# whose Cargo cache carries target/debug.
CI_MACOS_SHARD_1 := test-update-recovery-current test-port-forwarding \
	test-kubernetes-cli test-diagnostics test-release-gate test-workflows \
	test-guest-deployment test-hosted-validation
CI_MACOS_SHARD_1_USER := test-update-cli
CI_MACOS_SHARD_2 := test-update-ux-redirected test-update-native test-uninstall \
	test-update-check
CI_MACOS_SHARD_2_USER := test-update-ux-pty test-install-bootstrap \
	test-update-recovery-legacy
CI_MACOS_SHARD_3 := test-install-cleanup test-install-script \
	test-control-native test-profile-state test-release-artifacts
CI_MACOS_SHARD_3_USER :=
CI_MACOS_SHARD_4 := test-core-quality test-portable test-public-export \
	test-release-version test-release-request test-release-repository-preflight \
	test-control-tui test-control-rust
CI_MACOS_SHARD_4_USER := test-install-system-tools test-update-concurrency \
	test-update-properties
CI_MACOS_SHARD_5 := host test-update-script test-release-publish
CI_MACOS_SHARD_5_USER :=
CI_MACOS_LEAVES := $(foreach shard,$(CI_MACOS_SHARDS),$(CI_MACOS_SHARD_$(shard)) $(CI_MACOS_SHARD_$(shard)_USER))
CI_MACOS_SIDE_GATES := test-control-native test-control-tui \
	test-profile-state test-guest-deployment test-port-forwarding \
	test-kubernetes-cli test-diagnostics test-release-gate test-workflows
# test-update-check stays with the runner: its TUI fixtures start
# /usr/bin/python3, and under a freshly created user the TUI did not get
# their output before the test's deadline (run 36135535709).
CI_MACOS_USER_GATES := test-install-script test-install-cleanup \
	test-install-system-tools test-install-bootstrap test-uninstall \
	test-update-properties test-update-native test-update-concurrency \
	test-update-recovery-legacy test-update-recovery-current test-update-cli \
	test-update-ux-redirected test-update-ux-pty
CI_MACOS_ALONE_GATES := test-update-script test-release-artifacts \
	test-release-publish test-hosted-validation test-control-rust
ci_macos_side = $(filter $(CI_MACOS_SIDE_GATES),$(CI_MACOS_SHARD_$(1)))
ci_macos_alone = $(filter $(CI_MACOS_ALONE_GATES),$(CI_MACOS_SHARD_$(1)))
ci_macos_main = $(filter-out $(CI_MACOS_SIDE_GATES) $(CI_MACOS_ALONE_GATES),$(CI_MACOS_SHARD_$(1)))
ci_macos_user = $(CI_MACOS_SHARD_$(1)_USER)
ci_macos_parallel = $(or $(call ci_macos_side,$(1)),$(and $(CI_MACOS_LANE_UID),$(call ci_macos_user,$(1))))
# The second user (CI only): a numeric uid and its home directory.
CI_MACOS_LANE_UID ?=
CI_MACOS_LANE_HOME ?=
# One recipe line per gate, so a failure stops its lane and, with GNU Make
# 4's --output-sync=line, each gate's output appears when it finishes.
define ci-macos-gate
$(MAKE) $(1)

endef
# The second user gets a clean environment with this shell's PATH and runs
# the gate against the build/hamn already built (-o host).
define ci-macos-user-gate
$(if $(CI_MACOS_LANE_UID),sudo -u '#$(CI_MACOS_LANE_UID)' /usr/bin/env -i HOME=$(CI_MACOS_LANE_HOME) TMPDIR=/tmp PATH="$$PATH" LANG="$$LANG" $(MAKE) -o host $(1),$(MAKE) $(1))

endef
CI_MACOS_OUTPUT_SYNC := $(if $(filter output-sync,$(.FEATURES)),--output-sync=line)

test-local-macos:
	$(foreach gate,$(LOCAL_MACOS_GATES),$(MAKE) $(gate) &&) :

test-control test-install test-update:
	$(foreach part,$($@_PARTS),$(MAKE) $(part) &&) :

# Build build/hamn once, run the lanes together, then the alone gates. A
# shard without concurrent lanes stays outside a jobserver, so Cargo and the
# C core keep every CPU.
ci-macos-shard-%: check-ci-macos-shards
	@test -n "$(CI_MACOS_SHARD_$*)" || { echo "FAIL: unknown CI macOS shard: $*" >&2; exit 2; }
	$(MAKE) host
	$(MAKE) $(if $(call ci_macos_parallel,$*),-j3 $(CI_MACOS_OUTPUT_SYNC)) ci-macos-main-$* ci-macos-side-$* ci-macos-user-$*
	$(foreach gate,$(call ci_macos_alone,$*),$(call ci-macos-gate,$(gate)))

ci-macos-main-%:
	$(foreach gate,$(call ci_macos_main,$*),$(call ci-macos-gate,$(gate)))

ci-macos-side-%:
	$(foreach gate,$(call ci_macos_side,$*),$(call ci-macos-gate,$(gate)))

# Without a second user, the user lane must wait for the main lane.
ci-macos-user-%: $(if $(CI_MACOS_LANE_UID),,ci-macos-main-%)
	$(foreach gate,$(call ci_macos_user,$*),$(call ci-macos-user-gate,$(gate)))

check-ci-macos-shards:
	@test "$(sort $(CI_MACOS_LEAVES))" = "$(sort $(LOCAL_MACOS_LEAVES))" && \
		test "$(words $(CI_MACOS_LEAVES))" = "$(words $(LOCAL_MACOS_LEAVES))" || { \
		echo "FAIL: CI macOS shards must run every local macOS gate exactly once" >&2; exit 1; }
	@test -z "$(filter-out $(LOCAL_MACOS_LEAVES),$(CI_MACOS_SIDE_GATES) $(CI_MACOS_USER_GATES) $(CI_MACOS_ALONE_GATES))" || { \
		echo "FAIL: CI macOS lane classes name unknown gates" >&2; exit 1; }
	@test -z "$(filter-out $(CI_MACOS_USER_GATES),$(foreach shard,$(CI_MACOS_SHARDS),$(call ci_macos_user,$(shard))))" || { \
		echo "FAIL: a second-user lane holds a gate outside CI_MACOS_USER_GATES" >&2; exit 1; }

print-ci-macos-shards:
	@echo $(CI_MACOS_SHARDS)

test-port-forwarding: hamn-dev
	HAMN_DEV=$(HAMN_DEV) bash tests/host/test_port_forwarding.sh

test-qcow2: host
	@test -n "$(HAMN_QCOW2_IMAGE)" || { \
		echo "HAMN_QCOW2_IMAGE must name a signed guest image fixture" >&2; exit 2; \
	}
	HAMN=$(HOST_BIN) $(HAMN_DEV) test qcow2 $(HAMN_QCOW2_IMAGE)

test-profile-state: host $(LIFECYCLE_LOCK_TEST) $(CTLSOCK_TEST) $(FS_TEST) \
		$(SEED_MOUNTS_TEST) $(PROVISION_TEST) $(DEPLOYMENT_FINGERPRINT_TEST) \
		$(MANAGED_GUEST_IMAGE_TEST) $(SSH_OPTIONS_TEST) \
		$(START_DOCKER_CONTEXT_RETRY_TEST) $(RAW_CACHE_TEST)
	rm -rf $(BUILD)/tests/raw-cache-data && mkdir -p $(BUILD)/tests/raw-cache-data
	$(RAW_CACHE_TEST) $(BUILD)/tests/raw-cache-data
	rm -rf $(BUILD)/tests/raw-cache-data
	$(CTLSOCK_TEST)
	$(FS_TEST)
	$(SEED_MOUNTS_TEST)
	$(PROVISION_TEST)
	$(DEPLOYMENT_FINGERPRINT_TEST)
	$(MANAGED_GUEST_IMAGE_TEST)
	$(SSH_OPTIONS_TEST)
	$(START_DOCKER_CONTEXT_RETRY_TEST)
	$(LIFECYCLE_LOCK_TEST)
	HAMN=$(HOST_BIN) $(HAMN_DEV) test profile-yaml
	bash guest/tests/test_guest_deployment_transaction.sh

test-guest-deployment: host $(DEPLOYMENT_RECOVERY_TEST)
	$(MAKE) -C guest test-cri-status
	$(MAKE) -C guest test-mount-inotify
	bash guest/tests/test_guest_deployment_transaction.sh
	$(DEPLOYMENT_RECOVERY_TEST)
	bash guest/tests/test_make_install_targets.sh
	bash guest/tests/test_configure_docker.sh
	bash guest/tests/test_configure_rosetta.sh
	bash guest/tests/test_verify_image_contract.sh
	bash guest/tests/test_guest_image_builder.sh
	python3 guest/tests/test_image_size.py
	python3 guest/tests/test_image_evidence.py

test-diagnostics: host
	HAMN=$(HOST_BIN) $(HAMN_DEV) test diagnostics

test-install-script: host
	HAMN=$(HOST_BIN) bash tests/host/test_install.sh

test-install-cleanup: host
	HAMN=$(HOST_BIN) python3 tests/host/test_generation_cleanup.py

test-install-system-tools: host
	HAMN=$(HOST_BIN) python3 tests/host/test_install_system_tools.py

test-install-bootstrap: host
	python3 tests/host/test_bootstrap_acquire.py

test-uninstall: host
	HAMN=$(HOST_BIN) bash tests/host/test_uninstall.sh

test-update-properties: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_properties.py

test-update-native: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_native.py

test-update-concurrency: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_concurrency.py

test-update-recovery-legacy: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_recovery_ownership.py legacy

test-update-recovery-current: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_recovery_ownership.py current

test-update-check: host
	HAMN=$(HOST_BIN) python3 tests/host/test_update_check.py

test-update-cli: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_cli.py

test-update-ux-redirected: host
	HAMN=$(HOST_BIN) python3 tests/host/test_update_ux.py redirected

test-update-ux-pty: host
	HAMN=$(HOST_BIN) python3 tests/host/test_update_ux.py pty

test-update-script: host
	HAMN=$(HOST_BIN) bash tests/host/test_update.sh

# Candidate tests build their own release versions and never read the
# checkout's build/hamn, so they do not rebuild it first. The release
# drivers are hamn-dev subcommands run in this checkout; shell tests that
# still call them get hamn-dev as HAMN_DEV (an absolute path).
RELEASE_TOOL := HAMN_DEV=$(abspath $(HAMN_DEV))

test-release-artifacts: hamn-dev
	$(RELEASE_TOOL) bash tests/host/test_release_artifacts.sh

# hosted-validation builds a real candidate (replacing build/hamn, then
# restoring it); release-candidate checks the builder's inputs.
test-hosted-validation: hamn-dev
	$(HAMN_DEV) test hosted-validation
	$(HAMN_DEV) test release-candidate

test-release-gate: hamn-dev
	$(HAMN_DEV) test release-physical
	$(HAMN_DEV) test release-gate

test-release-publish: hamn-dev
	$(RELEASE_TOOL) bash tests/host/test_release_publish.sh

test-release-version: hamn-dev
	$(HAMN_DEV) test release-version
	$(HAMN_DEV) test release-github
	$(HAMN_DEV) test release-network

test-release-request: hamn-dev
	$(HAMN_DEV) test release-request

test-public-export: hamn-dev
	$(HAMN_DEV) test public-export

test-release-repository-preflight: hamn-dev
	$(HAMN_DEV) test release-repository-preflight

release-candidate: hamn-dev
	$(HAMN_DEV) release build-candidate

release-hosted-validation: hamn-dev
	$(HAMN_DEV) release hosted-validation

# The physical harness is hamn-dev built here from the checkout that the
# gate then requires to be clean and to match the candidate's source.
release-gate: hamn-dev
	$(HAMN_DEV) release gate

test-kubernetes-cli: host
	HAMN=$(HOST_BIN) $(HAMN_DEV) test kubernetes-api
	bash guest/tests/test_configure_containerd.sh

test-core-quality: host
	bash tests/host/test_core_quality.sh

clean:
	rm -rf $(BUILD)
