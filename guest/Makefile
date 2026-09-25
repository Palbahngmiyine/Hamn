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

# Strict JSON used by guest-side C tools such as the in-guest helper.
STRICT_JSON := json/strict_json.c json/strict_json.h
GUEST_JSON_BIN := build/guest-json

STRICT_JSON_TEST := $(TEST_BUILD)/test_strict_json
GUEST_TEST_FIXTURE := $(TEST_BUILD)/guest-test-fixture

all: $(AGENT_BIN) $(GUEST_JSON_BIN)

$(AGENT_BIN): $(AGENT_SRCS) Makefile
	@mkdir -p build
	$(CC) -Iagent $(CFLAGS) $(AGENT_SRCS) -pthread -o $@

$(GUEST_JSON_BIN): json/guest_json.c $(STRICT_JSON) Makefile
	@mkdir -p build
	$(CC) $(CFLAGS) json/guest_json.c json/strict_json.c -lm -o $@

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

test-strict-json: $(STRICT_JSON_TEST)
	$(STRICT_JSON_TEST)

.PHONY: all install install-common clean test-agent test-cri-status \
	test-mount-inotify test-deployment-transaction test-strict-json
