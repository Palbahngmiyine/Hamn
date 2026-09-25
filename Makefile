BUILD      := build
HOST_BIN   := $(BUILD)/hamn
CARGO_PROFILE ?= release
# x-release-please-start-version
VERSION    ?= 0.1.2
# x-release-please-end
VERSION_STAMP := $(BUILD)/.hamn-version
PREFIX     ?= $(HOME)/.local
BINDIR     ?= $(PREFIX)/bin
DATADIR    ?= $(PREFIX)/share/hamn/src

MACOS_MIN  := 13.0
CFLAGS     := -std=c11 -Wall -Wextra -O2 -g \
              -Werror=implicit-function-declaration \
              -MMD -MP \
              -mmacosx-version-min=$(MACOS_MIN) \
              -DHAVE_CONFIG_H \
              -DHAMN_VERSION=\"$(VERSION)\" \
              -Ihost -Ivendor -Ivendor/libyaml/include -I$(BUILD)/generated
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
LIFECYCLE_LOCK_TEST := $(BUILD)/tests/test_lifecycle_lock
CTLSOCK_TEST := $(BUILD)/tests/test_ctlsock
FS_TEST := $(BUILD)/tests/test_fs
SEED_MOUNTS_TEST := $(BUILD)/tests/test_cloudinit_mounts
PROVISION_TEST := $(BUILD)/tests/test_provision
DEPLOYMENT_FINGERPRINT_TEST := $(BUILD)/tests/test_guest_deployment_fingerprint
MANAGED_GUEST_IMAGE_TEST := $(BUILD)/tests/test_managed_guest_image
SSH_OPTIONS_TEST := $(BUILD)/tests/test_ssh_options
PROFILE_READ_TEST := $(BUILD)/tests/test_profile_read
START_DOCKER_CONTEXT_RETRY_TEST := $(BUILD)/tests/test_start_docker_context_retry

-include $(HOST_DEPS)

.PHONY: FORCE host install clean test-portable test-qcow2 test-control \
	test-profile-state test-guest-deployment test-diagnostics test-install \
	test-uninstall test-update test-release-artifacts test-hosted-validation \
	test-release-gate test-release-publish \
	test-release-version \
	test-release-request \
	test-kubernetes-cli test-core-quality test-public-export test-release-repository-preflight \
	test-port-forwarding test-workflows test-local-macos \
	$(test-control_PARTS) $(test-install_PARTS) $(test-update_PARTS) \
	check-ci-macos-shards print-ci-macos-shards \
	release-candidate release-gate release-hosted-validation

FORCE:

host: $(HOST_BIN)

$(VERSION_STAMP): FORCE
	@mkdir -p $(dir $@)
	@if ! test -f $@ || ! test "$$(cat $@)" = "$(VERSION)"; then \
		printf '%s\n' "$(VERSION)" > $@.tmp; \
		mv $@.tmp $@; \
	fi

$(BUILD)/generated/k3s_retirement.h: scripts/embed-retirement.py host/migration/retire_k3s.py host/migration/legacy-k3s.service guest/scripts/verify-image-contract.sh guest/scripts/guest-deployment-transaction.sh guest/scripts/configure-docker.sh
	python3 scripts/embed-retirement.py $@

$(HOST_OBJS): $(VERSION_STAMP) $(BUILD)/generated/k3s_retirement.h

install: host
	bash scripts/install-host.sh "$(HOST_BIN)" "$(BINDIR)" "$(DATADIR)"

$(BUILD)/libhamn_core.a: $(HOST_OBJS)
	@mkdir -p $(dir $@)
	rm -f $@.tmp
	ar rcs $@.tmp $(HOST_OBJS)
	mv $@.tmp $@

$(HOST_BIN): FORCE host/entitlements.plist
	MACOSX_DEPLOYMENT_TARGET=$(MACOS_MIN) python3 scripts/build-host.py $@ $(VERSION) $(CARGO_PROFILE)

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

$(DEPLOYMENT_FINGERPRINT_TEST): tests/host/test_guest_deployment_fingerprint.c \
		$(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(MANAGED_GUEST_IMAGE_TEST): tests/host/test_managed_guest_image.c \
		$(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(SSH_OPTIONS_TEST): tests/host/test_ssh_options.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(START_DOCKER_CONTEXT_RETRY_TEST): tests/host/test_start_docker_context_retry.c \
		host/cmd/cmd_start.c $(HOST_C_SRCS)
	@mkdir -p $(dir $@)
	clang -DHAMN_TEST $(filter-out -MMD -MP,$(CFLAGS)) $< \
		$(filter-out host/main.c host/cmd/cmd_start.c,$(HOST_C_SRCS)) \
		host/cmd/cmd_start.c $(HOST_M_SRCS) $(LDFLAGS) -o $@

test-portable:
	bash tests/ci/test_portable.sh

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

# These share one debug test build: the flag inventory and test_tui.py run
# `cargo test` binaries themselves, so they stay with `cargo test --locked`.
test-control-rust: host
	python3 tests/host/test_native_flag_inventory.py
	cargo test --locked
	@test "$$(cargo tree --locked --prefix none --format '{p}' | sed -n '/^crossterm v/p' | cut -d ' ' -f 1,2 | sort -u | wc -l | tr -d ' ')" = 1
	HAMN=$(HOST_BIN) python3 tests/host/test_tui.py

test-control-native: host $(PROFILE_READ_TEST) $(BUILD)/tests/test_docker_readiness $(BUILD)/tests/test_proc_deadline $(BUILD)/tests/test_ssh_deadline
	python3 tests/host/test_control_signed_bootstrap.py
	python3 tests/host/test_tui_create_plugins.py
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_operation.c host/core/operation.c host/core/log.c host/util/fs.c vendor/cjson/cJSON.c -o $(BUILD)/tests/test_operation
	$(BUILD)/tests/test_operation
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_operation_preflight.c host/core/operation.c host/core/log.c host/util/fs.c vendor/cjson/cJSON.c -o $(BUILD)/tests/test_operation_preflight
	$(BUILD)/tests/test_operation_preflight
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_remote_mutation.c host/core/remote_mutation.c -o $(BUILD)/tests/test_remote_mutation
	$(BUILD)/tests/test_remote_mutation
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_proc_early_exit.c -o $(BUILD)/tests/test_proc_early_exit
	$(BUILD)/tests/test_proc_early_exit
	python3 tests/host/test_remote_cancel_boundaries.py
	python3 tests/host/workspace_live_transport.py
	HAMN=$(HOST_BIN) python3 tests/host/test_start_preflight.py
	clang $(filter-out -MMD -MP,$(CFLAGS)) tests/host/test_proc_cancellation.c host/util/proc.c -o $(BUILD)/tests/test_proc_cancellation
	$(BUILD)/tests/test_proc_cancellation
	$(BUILD)/tests/test_proc_deadline
	SSH_DEADLINE_TEST=$(BUILD)/tests/test_ssh_deadline python3 tests/host/test_ssh_deadline.py
	python3 tests/host/test_docker_readiness.py
	$(PROFILE_READ_TEST)
	HAMN=$(HOST_BIN) python3 tests/host/test_core_worker.py
	HAMN=$(HOST_BIN) python3 tests/host/test_docker_api.py
	HAMN=$(HOST_BIN) python3 tests/host/test_docker_context.py
	HAMN=$(HOST_BIN) python3 tests/host/test_kubernetes_api.py
	HAMN=$(HOST_BIN) python3 tests/host/test_exec_auth.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_navigation.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_review_improvements.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_session_management.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_workspaces.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_outcomes.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_native_regressions.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_quoting.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_reload.py
	HAMN=$(HOST_BIN) python3 tests/host/test_native_query_lifetime.py
	python3 tests/host/test_workspace_live_prepare.py
	HAMN=$(HOST_BIN) python3 tests/host/test_workspace_management.py
	python3 tests/host/test_workspace_live_external_contexts.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_docker_all.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_docker_images.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_docker_config.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_kubectl_output.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_cluster_target.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_picker_restore.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_environment_actions.py
	python3 tests/host/test_workspace_kubernetes_assertions.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_tls_target.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_backpressure.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_guarded_delete.py
	HAMN=$(HOST_BIN) python3 tests/host/test_tui_ssh_timeout.py
	python3 tests/host/test_k3s_retirement.py
	python3 tests/host/test_build_publish.py
	python3 tests/host/test_rust_sdk.py
	HAMN=$(HOST_BIN) bash tests/host/test_single_binary.sh

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
test-control_PARTS := test-control-native test-control-rust
test-install_PARTS := test-install-script test-install-cleanup \
	test-install-system-tools test-install-bootstrap
test-update_PARTS := test-update-properties test-update-native \
	test-update-concurrency test-update-recovery test-update-check \
	test-update-cli test-update-ux test-update-script
LOCAL_MACOS_LEAVES := $(foreach gate,$(LOCAL_MACOS_GATES),$(or $($(gate)_PARTS),$(gate)))

# PR CI runs the same leaves on independent macOS machines, each shard
# serially in its own checkout (installer tests must not share a machine:
# generation cleanup scans every process of the user). Balance by measured
# durations; check-ci-macos-shards proves the shards partition the leaves.
CI_MACOS_SHARDS := 1 2 3 4 5
CI_MACOS_SHARD_1 := test-update-ux test-install-script test-portable \
	test-diagnostics test-uninstall test-release-gate test-update-native
CI_MACOS_SHARD_2 := test-control-native test-guest-deployment \
	test-update-properties test-install-bootstrap test-core-quality
CI_MACOS_SHARD_3 := test-update-recovery test-hosted-validation \
	test-kubernetes-cli test-update-check
CI_MACOS_SHARD_4 := test-control-rust test-update-script test-release-artifacts \
	test-install-system-tools test-release-version test-workflows
CI_MACOS_SHARD_5 := host test-update-cli test-install-cleanup test-profile-state \
	test-update-concurrency test-release-publish test-port-forwarding \
	test-release-request test-public-export test-release-repository-preflight
CI_MACOS_LEAVES := $(foreach shard,$(CI_MACOS_SHARDS),$(CI_MACOS_SHARD_$(shard)))

test-local-macos:
	$(foreach gate,$(LOCAL_MACOS_GATES),$(MAKE) $(gate) &&) :

test-control test-install test-update:
	$(foreach part,$($@_PARTS),$(MAKE) $(part) &&) :

ci-macos-shard-%: check-ci-macos-shards
	@test -n "$(CI_MACOS_SHARD_$*)" || { echo "FAIL: unknown CI macOS shard: $*" >&2; exit 2; }
	$(foreach gate,$(CI_MACOS_SHARD_$*),$(MAKE) $(gate) &&) :

check-ci-macos-shards:
	@test "$(sort $(CI_MACOS_LEAVES))" = "$(sort $(LOCAL_MACOS_LEAVES))" && \
		test "$(words $(CI_MACOS_LEAVES))" = "$(words $(LOCAL_MACOS_LEAVES))" || { \
		echo "FAIL: CI macOS shards must run every local macOS gate exactly once" >&2; exit 1; }

print-ci-macos-shards:
	@echo $(CI_MACOS_SHARDS)

test-port-forwarding:
	bash tests/host/test_port_forwarding.sh

test-qcow2: host
	@test -n "$(HAMN_QCOW2_IMAGE)" || { \
		echo "HAMN_QCOW2_IMAGE must name a signed guest image fixture" >&2; exit 2; \
	}
	HAMN=$(HOST_BIN) bash tests/host/test_qcow2.sh $(HAMN_QCOW2_IMAGE)

test-profile-state: host $(LIFECYCLE_LOCK_TEST) $(CTLSOCK_TEST) $(FS_TEST) \
		$(SEED_MOUNTS_TEST) $(PROVISION_TEST) $(DEPLOYMENT_FINGERPRINT_TEST) \
		$(MANAGED_GUEST_IMAGE_TEST) $(SSH_OPTIONS_TEST) \
		$(START_DOCKER_CONTEXT_RETRY_TEST)
	bash tests/host/test_raw_cache.sh
	$(CTLSOCK_TEST)
	$(FS_TEST)
	$(SEED_MOUNTS_TEST)
	$(PROVISION_TEST)
	$(DEPLOYMENT_FINGERPRINT_TEST)
	$(MANAGED_GUEST_IMAGE_TEST)
	$(SSH_OPTIONS_TEST)
	$(START_DOCKER_CONTEXT_RETRY_TEST)
	HAMN=$(HOST_BIN) LIFECYCLE_LOCK_TEST=$(LIFECYCLE_LOCK_TEST) \
		bash tests/host/test_profile_yaml.sh
	bash guest/tests/test_guest_deployment_transaction.sh

test-guest-deployment: host
	$(MAKE) -C guest test-cri-status
	$(MAKE) -C guest test-mount-inotify
	bash guest/tests/test_guest_deployment_transaction.sh
	bash guest/tests/test_make_install_targets.sh
	bash guest/tests/test_configure_docker.sh
	bash guest/tests/test_configure_rosetta.sh
	bash guest/tests/test_verify_image_contract.sh
	bash guest/tests/test_guest_image_builder.sh
	python3 guest/tests/test_image_size.py
	python3 guest/tests/test_image_evidence.py

test-diagnostics: host
	HAMN=$(HOST_BIN) bash tests/host/test_diagnostics.sh

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

test-update-recovery: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_recovery_ownership.py

test-update-check: host
	HAMN=$(HOST_BIN) python3 tests/host/test_update_check.py

test-update-cli: host
	HAMN=$(HOST_BIN) python3 tests/host/test_upgrade_cli.py

test-update-ux: host
	HAMN=$(HOST_BIN) python3 tests/host/test_update_ux.py

test-update-script: host
	HAMN=$(HOST_BIN) bash tests/host/test_update.sh

# Candidate tests build their own release versions and never read the
# checkout's build/hamn, so they do not rebuild it first.
test-release-artifacts:
	bash tests/host/test_release_artifacts.sh

test-hosted-validation:
	bash tests/host/test_hosted_validation.sh

test-release-gate:
	bash tests/host/test_release_gate.sh

test-release-publish:
	bash tests/host/test_release_publish.sh

test-release-version:
	bash tests/host/test_release_version.sh
	python3 tests/host/test_release_completion.py
	python3 tests/host/test_release_pr_ready.py
	bash tests/host/test_release_network.sh

test-release-request:
	bash tests/host/test_release_request.sh

test-public-export:
	bash tests/host/test_public_export.sh

test-release-repository-preflight:
	bash tests/host/test_release_repository_preflight.sh

release-candidate:
	bash packaging/release/build-candidate.sh

release-hosted-validation:
	bash packaging/release/hosted-validation.sh

release-gate:
	bash packaging/release/release-gate.sh

test-kubernetes-cli: host
	HAMN=$(HOST_BIN) python3 tests/host/test_kubernetes_api.py
	python3 tests/host/test_k3s_retirement.py
	bash guest/tests/test_configure_containerd.sh

test-core-quality: host
	bash tests/host/test_core_quality.sh

clean:
	rm -rf $(BUILD)
