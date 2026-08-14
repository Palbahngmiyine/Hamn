BUILD      := build
HOST_BIN   := $(BUILD)/hamn
VERSION    ?= 0.0.1
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
              -Ihost -Ivendor -Ivendor/libyaml/include
OBJCFLAGS  := $(filter-out -std=c11,$(CFLAGS)) -fobjc-arc
LDFLAGS    := -framework Virtualization -framework Foundation -framework CoreServices -lz \
              -mmacosx-version-min=$(MACOS_MIN)

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
KUBECONFIG_CONTEXT_TEST := $(BUILD)/tests/test_kubeconfig_context
KUBERNETES_TRANSACTION_TEST := $(BUILD)/tests/test_kubernetes_transaction
START_DOCKER_CONTEXT_RETRY_TEST := $(BUILD)/tests/test_start_docker_context_retry

-include $(HOST_DEPS)

.PHONY: FORCE host install clean test-portable test-qcow2 \
	test-profile-state test-guest-deployment test-diagnostics test-install \
	test-uninstall test-update test-release-artifacts test-release-gate test-release-publish \
	test-kubernetes-cli test-core-quality test-public-export test-release-repository-preflight \
	test-port-forwarding test-workflows test-local-macos \
	release-candidate release-gate

FORCE:

host: $(HOST_BIN)

$(VERSION_STAMP): FORCE
	@mkdir -p $(dir $@)
	@if ! test -f $@ || ! test "$$(cat $@)" = "$(VERSION)"; then \
		printf '%s\n' "$(VERSION)" > $@.tmp; \
		mv $@.tmp $@; \
	fi

$(HOST_OBJS): $(VERSION_STAMP)

install: host
	bash scripts/install-host.sh "$(HOST_BIN)" "$(BINDIR)" "$(DATADIR)"

$(HOST_BIN): $(HOST_OBJS) host/entitlements.plist
	@mkdir -p $(dir $@)
	clang $(HOST_OBJS) $(LDFLAGS) -o $@
	codesign --force --sign - --entitlements host/entitlements.plist $@

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

$(KUBECONFIG_CONTEXT_TEST): tests/host/test_kubeconfig_context.c $(HOST_TEST_OBJS)
	@mkdir -p $(dir $@)
	clang $(filter-out -MMD -MP,$(CFLAGS)) $< $(HOST_TEST_OBJS) \
		$(LDFLAGS) -o $@

$(KUBERNETES_TRANSACTION_TEST): tests/host/test_kubernetes_transaction.c \
		host/cmd/cmd_kubernetes.c $(HOST_C_SRCS)
	@mkdir -p $(dir $@)
	clang -DHAMN_TEST $(filter-out -MMD -MP,$(CFLAGS)) $< \
		$(filter-out host/main.c host/cmd/cmd_kubernetes.c,$(HOST_C_SRCS)) \
		host/cmd/cmd_kubernetes.c $(HOST_M_SRCS) $(LDFLAGS) -o $@

$(START_DOCKER_CONTEXT_RETRY_TEST): tests/host/test_start_docker_context_retry.c \
		host/cmd/cmd_start.c $(HOST_C_SRCS)
	@mkdir -p $(dir $@)
	clang -DHAMN_TEST $(filter-out -MMD -MP,$(CFLAGS)) $< \
		$(filter-out host/main.c host/cmd/cmd_start.c,$(HOST_C_SRCS)) \
		host/cmd/cmd_start.c $(HOST_M_SRCS) $(LDFLAGS) -o $@

test-portable:
	bash tests/ci/test_portable.sh

test-workflows:
	@command -v actionlint >/dev/null || { \
		echo "actionlint is required for test-workflows" >&2; exit 1; \
	}
	actionlint -config-file .github/actionlint.yaml .github/workflows/*.yml

test-local-macos:
	$(MAKE) test-workflows
	$(MAKE) test-portable
	$(MAKE) test-core-quality
	$(MAKE) test-port-forwarding
	$(MAKE) host
	$(MAKE) test-profile-state
	$(MAKE) test-guest-deployment
	$(MAKE) test-diagnostics
	$(MAKE) test-install
	$(MAKE) test-uninstall
	$(MAKE) test-update
	$(MAKE) test-kubernetes-cli
	$(MAKE) test-release-artifacts
	$(MAKE) test-release-gate
	$(MAKE) test-release-publish
	$(MAKE) test-public-export
	$(MAKE) test-release-repository-preflight

test-port-forwarding:
	bash tests/host/test_port_forwarding.sh

test-qcow2: host
	@test -n "$(HAMN_QCOW2_IMAGE)" || { \
		echo "HAMN_QCOW2_IMAGE must name a signed guest image fixture" >&2; exit 2; \
	}
	HAMN=$(HOST_BIN) bash tests/host/test_qcow2.sh $(HAMN_QCOW2_IMAGE)

test-profile-state: host $(LIFECYCLE_LOCK_TEST) $(CTLSOCK_TEST) $(FS_TEST) \
		$(SEED_MOUNTS_TEST) $(PROVISION_TEST) $(DEPLOYMENT_FINGERPRINT_TEST) \
		$(MANAGED_GUEST_IMAGE_TEST) $(SSH_OPTIONS_TEST) $(KUBECONFIG_CONTEXT_TEST) \
		$(START_DOCKER_CONTEXT_RETRY_TEST)
	$(CTLSOCK_TEST)
	$(FS_TEST)
	$(SEED_MOUNTS_TEST)
	$(PROVISION_TEST)
	$(DEPLOYMENT_FINGERPRINT_TEST)
	$(MANAGED_GUEST_IMAGE_TEST)
	$(SSH_OPTIONS_TEST)
	$(KUBECONFIG_CONTEXT_TEST)
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

test-diagnostics: host
	HAMN=$(HOST_BIN) bash tests/host/test_diagnostics.sh

test-install: host
	HAMN=$(HOST_BIN) bash tests/host/test_install.sh

test-uninstall: host
	HAMN=$(HOST_BIN) bash tests/host/test_uninstall.sh

test-update: host
	HAMN=$(HOST_BIN) bash tests/host/test_update.sh

test-release-artifacts: host
	bash tests/host/test_release_artifacts.sh

test-release-gate: host
	bash tests/host/test_release_gate.sh

test-release-publish: host
	bash tests/host/test_release_publish.sh

test-public-export:
	bash tests/host/test_public_export.sh

test-release-repository-preflight:
	bash tests/host/test_release_repository_preflight.sh

release-candidate:
	bash packaging/release/build-candidate.sh

release-gate:
	bash packaging/release/release-gate.sh

test-kubernetes-cli: host $(KUBERNETES_TRANSACTION_TEST)
	HAMN=$(HOST_BIN) bash tests/host/test_kubernetes_profile.sh
	$(KUBERNETES_TRANSACTION_TEST)
	bash guest/tests/test_install_k3s.sh
	bash guest/tests/test_configure_containerd.sh
	bash guest/tests/test_k3s_configuration.sh

test-core-quality: host
	bash tests/host/test_core_quality.sh

clean:
	rm -rf $(BUILD)
