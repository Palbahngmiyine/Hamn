# hamnd는 Docker/CRI 상태만 보고하는 immutable guest-image 에이전트다.
# 컨테이너 엔진은 Docker daemon과 system containerd이며, host는 guest 소스를
# virtiofs로 전달하거나 런타임에 agent를 재설치하지 않는다.

CC      ?= gcc
CFLAGS  := -std=gnu11 -O2 -g -Wall -Wextra \
           -Werror=implicit-function-declaration -D_GNU_SOURCE \
           -Iagent -I../vendor

AGENT_SRCS := agent/main.c agent/api/cri_status.c agent/api/mount_inotify.c \
              agent/api/router.c \
              agent/http/conn.c agent/http/server.c agent/http/resp.c \
              agent/loop/loop.c ../vendor/cjson/cJSON.c \
              ../vendor/picohttpparser/picohttpparser.c

AGENT_BIN := build/hamnd-agent
CRI_STATUS_TEST := build/test_cri_status

all: $(AGENT_BIN)

$(AGENT_BIN): $(AGENT_SRCS) Makefile
	@mkdir -p build
	$(CC) -Iagent $(CFLAGS) $(AGENT_SRCS) -pthread -o $@

install: install-common

install-common: $(AGENT_BIN)
	install -d -m 0755 $(DESTDIR)/usr/local/bin
	install -m 0755 $(AGENT_BIN) $(DESTDIR)/usr/local/bin/hamnd
	install -d -m 0755 $(DESTDIR)/usr/local/libexec/hamn
	install -m 0755 scripts/configure-containerd.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-containerd
	install -m 0755 scripts/configure-docker.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-docker
	install -m 0755 scripts/configure-rosetta.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-rosetta
	install -m 0755 scripts/verify-image-contract.sh \
		$(DESTDIR)/usr/local/libexec/hamn/verify-image-contract
	install -m 0755 scripts/configure-k3s.sh \
		$(DESTDIR)/usr/local/libexec/hamn/configure-k3s
	install -m 0755 scripts/install-k3s.sh \
		$(DESTDIR)/usr/local/libexec/hamn/install-k3s
	install -m 0755 scripts/guest-deployment-transaction.sh \
		$(DESTDIR)/usr/local/libexec/hamn/guest-deployment-transaction
	install -d -m 0755 $(DESTDIR)/etc/systemd/system
	install -m 0644 systemd/hamnd.service \
		$(DESTDIR)/etc/systemd/system/hamnd.service
	install -m 0644 systemd/k3s.service \
		$(DESTDIR)/etc/systemd/system/k3s.service

clean:
	rm -rf build

test-agent: $(AGENT_BIN)
	bash tests/test_agent.sh $(AGENT_BIN)

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

.PHONY: all install install-common clean test-agent test-cri-status test-mount-inotify \
	test-deployment-transaction
