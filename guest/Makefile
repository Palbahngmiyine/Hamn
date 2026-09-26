# hamnd는 Docker/CRI 상태만 보고하는 immutable guest-image 에이전트다.
# 컨테이너 엔진은 Docker daemon과 system containerd이며, host는 guest 소스를
# virtiofs로 전달하거나 런타임에 agent를 재설치하지 않는다.

CC      ?= gcc
CFLAGS  := -std=gnu11 -O2 -g -Wall -Wextra \
           -Werror=implicit-function-declaration -D_GNU_SOURCE \
           -Iagent -I../vendor
# Extra flags for test programs only, e.g. SANITIZE=-fsanitize=address,undefined
# with TEST_BUILD=build/sanitize so instrumented and normal objects never mix.
SANITIZE ?=
TEST_BUILD ?= build

AGENT_SRCS := agent/main.c agent/api/cri_status.c agent/api/mount_inotify.c \
              agent/api/router.c \
              agent/http/conn.c agent/http/server.c agent/http/resp.c \
              agent/loop/loop.c ../vendor/cjson/cJSON.c \
              ../vendor/picohttpparser/picohttpparser.c

AGENT_BIN := build/hamnd-agent
AGENT_TEST_BIN := build/hamnd-agent-test
CRI_STATUS_TEST := build/test_cri_status

# Strict JSON used by both the in-guest helper and the image-builder tool.
STRICT_JSON := json/strict_json.c json/strict_json.h
GUEST_JSON_BIN := build/guest-json

# Image evidence and size gates run on the Linux image builder and in release
# publication; they need only a C compiler and libc. The builder passes an
# absolute IMAGE_TOOL so it never writes into its source checkout.
IMAGE_TOOL ?= build/hamn-image-tool
IMAGE_TOOL_SRCS := image/image_util.c image/digest.c image/evidence.c \
                   image/size_gate.c image/raw_check.c image/variations.c \
                   json/strict_json.c
IMAGE_TOOL_HDRS := image/image_util.h image/digest.h image/evidence.h \
                   image/size_gate.h image/raw_check.h image/variations.h \
                   json/strict_json.h

STRICT_JSON_TEST := $(TEST_BUILD)/test_strict_json
IMAGE_TOOL_TEST := $(TEST_BUILD)/test_image_tool
IMAGE_TOOL_UNDER_TEST := $(TEST_BUILD)/hamn-image-tool
GUEST_TEST_FIXTURE := $(TEST_BUILD)/guest-test-fixture

all: $(AGENT_BIN) $(GUEST_JSON_BIN)

$(AGENT_BIN): $(AGENT_SRCS) Makefile
	@mkdir -p build
	$(CC) -Iagent $(CFLAGS) $(AGENT_SRCS) -pthread -o $@

$(GUEST_JSON_BIN): json/guest_json.c $(STRICT_JSON) Makefile
	@mkdir -p build
	$(CC) $(CFLAGS) json/guest_json.c json/strict_json.c -lm -o $@

image-tool: $(IMAGE_TOOL)

$(IMAGE_TOOL): image/image_tool.c $(IMAGE_TOOL_SRCS) $(IMAGE_TOOL_HDRS) Makefile
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) -I. image/image_tool.c $(IMAGE_TOOL_SRCS) -lm -o $@

install: install-common

install-common: $(AGENT_BIN) $(GUEST_JSON_BIN)
	install -d -m 0755 $(DESTDIR)/usr/local/bin
	install -m 0755 $(AGENT_BIN) $(DESTDIR)/usr/local/bin/hamnd
	install -d -m 0755 $(DESTDIR)/usr/local/libexec/hamn
	install -m 0755 $(GUEST_JSON_BIN) \
		$(DESTDIR)/usr/local/libexec/hamn/guest-json
	install -m 0755 scripts/configure-containerd.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-containerd
	install -m 0755 scripts/configure-docker.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-docker
	install -m 0755 scripts/configure-rosetta.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-rosetta
	install -m 0755 scripts/verify-image-contract.sh \
		$(DESTDIR)/usr/local/libexec/hamn/verify-image-contract
	install -m 0755 scripts/guest-deployment-transaction.sh \
		$(DESTDIR)/usr/local/libexec/hamn/guest-deployment-transaction
	install -d -m 0755 $(DESTDIR)/etc/systemd/system
	install -m 0644 systemd/hamnd.service \
		$(DESTDIR)/etc/systemd/system/hamnd.service

clean:
	rm -rf build

$(AGENT_TEST_BIN): $(AGENT_SRCS) Makefile
	@mkdir -p build
	$(CC) $(CFLAGS) -DHAMN_TEST $(AGENT_SRCS) -pthread -o $@

$(GUEST_TEST_FIXTURE): tests/guest_test_fixture.c $(STRICT_JSON) Makefile
	@mkdir -p $(TEST_BUILD)
	$(CC) $(CFLAGS) $(SANITIZE) -I. tests/guest_test_fixture.c \
		json/strict_json.c -lm -o $@

test-agent: $(AGENT_TEST_BIN) $(GUEST_TEST_FIXTURE)
	bash tests/test_agent.sh $(AGENT_TEST_BIN) $(GUEST_TEST_FIXTURE)

$(CRI_STATUS_TEST): tests/test_cri_status.c agent/api/cri_status.c \
		agent/api/cri_status.h Makefile
	@mkdir -p build
	$(CC) $(CFLAGS) -DHAMN_TEST tests/test_cri_status.c \
		agent/api/cri_status.c -o $@

test-cri-status: $(CRI_STATUS_TEST)
	bash tests/test_cri_status.sh $(CRI_STATUS_TEST)

test-mount-inotify:
	bash tests/test_mount_inotify.sh

test-deployment-transaction:
	bash tests/test_guest_deployment_transaction.sh

$(STRICT_JSON_TEST): tests/test_strict_json.c $(STRICT_JSON) Makefile
	@mkdir -p $(TEST_BUILD)
	$(CC) $(CFLAGS) $(SANITIZE) -I. tests/test_strict_json.c \
		json/strict_json.c -lm -o $@

$(IMAGE_TOOL_TEST): tests/test_image_tool.c $(IMAGE_TOOL_SRCS) \
		$(IMAGE_TOOL_HDRS) Makefile
	@mkdir -p $(TEST_BUILD)
	$(CC) $(CFLAGS) $(SANITIZE) -DHAMN_TEST -I. tests/test_image_tool.c \
		$(IMAGE_TOOL_SRCS) -lm -o $@

ifneq ($(IMAGE_TOOL_UNDER_TEST),$(IMAGE_TOOL))
$(IMAGE_TOOL_UNDER_TEST): image/image_tool.c $(IMAGE_TOOL_SRCS) \
		$(IMAGE_TOOL_HDRS) Makefile
	@mkdir -p $(TEST_BUILD)
	$(CC) $(CFLAGS) $(SANITIZE) -I. image/image_tool.c $(IMAGE_TOOL_SRCS) \
		-lm -o $@
endif

test-strict-json: $(STRICT_JSON_TEST)
	$(STRICT_JSON_TEST)

test-image-tools: $(IMAGE_TOOL_TEST) $(IMAGE_TOOL_UNDER_TEST)
	$(IMAGE_TOOL_TEST)
	bash tests/test_image_tools.sh $(IMAGE_TOOL_UNDER_TEST)

.PHONY: all install install-common clean image-tool test-agent test-cri-status \
	test-mount-inotify test-deployment-transaction test-strict-json \
	test-image-tools
