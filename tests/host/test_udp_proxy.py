#!/usr/bin/env python3
from contextlib import contextmanager
import os
from pathlib import Path
import signal
import selectors
import socket
import subprocess
import sys
import threading
import time

FAULT_VARIABLES = (
    "HAMN_TEST_UDP_SEND_FAILURE",
    "HAMN_TEST_UDP_RECV_FAILURE",
    "HAMN_TEST_UDP_POLL_FAILURE",
)
FLOW_LIMIT = 64


def fail(message):
    raise SystemExit(f"FAIL: {message}")


def wait_for_pidfile(process, pidfile):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if pidfile.is_file():
            return
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            fail(
                f"UDP relay exited before readiness: rc={process.returncode}, "
                f"stdout={stdout!r}, stderr={stderr!r}"
            )
        time.sleep(0.01)
    process.kill()
    process.wait()
    fail("UDP relay did not publish its pidfile within 5s")


def spawn_relay(binary, work, target_port, environment=None):
    listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listener.bind(("127.0.0.1", 0))
    listen_port = listener.getsockname()[1]
    listener.set_inheritable(True)
    pidfile = work / f"relay-{listen_port}.pid"
    command = [
        binary,
        "udp-forward",
        "--listen-address",
        "127.0.0.1",
        "--listen-port",
        str(listen_port),
        "--listen-fd",
        str(listener.fileno()),
        "--target-address",
        "127.0.0.1",
        "--target-port",
        str(target_port),
        "--pidfile",
        str(pidfile),
    ]
    process = subprocess.Popen(
        command,
        pass_fds=(listener.fileno(),),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment or fault_environment(),
    )
    listener.close()
    try:
        wait_for_pidfile(process, pidfile)
    except BaseException:
        if process.poll() is None:
            process.kill()
        process.wait()
        raise
    return process, listen_port, pidfile


def stop_relay(process, pidfile):
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
    try:
        stdout, stderr = process.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        stdout, stderr = process.communicate()
        fail(
            f"UDP relay did not stop within 5s: "
            f"stdout={stdout!r}, stderr={stderr!r}"
        )
    if process.returncode != 0:
        fail(
            f"UDP relay shutdown failed: rc={process.returncode}, "
            f"stdout={stdout!r}, stderr={stderr!r}"
        )
    if pidfile.exists():
        fail("UDP relay pidfile remained after shutdown")


@contextmanager
def running_relay(binary, work, target_port, environment=None):
    process, listen_port, pidfile = spawn_relay(
        binary, work, target_port, environment
    )
    try:
        yield process, listen_port
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGCONT)
        stop_relay(process, pidfile)


def wait_for_stderr(process, expected):
    selector = selectors.DefaultSelector()
    selector.register(process.stderr, selectors.EVENT_READ)
    deadline = time.monotonic() + 5
    output = b""
    try:
        while expected not in output:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                fail(f"UDP relay did not report {expected!r}: {output!r}")
            if not selector.select(remaining):
                fail(f"UDP relay did not report {expected!r}: {output!r}")
            chunk = os.read(process.stderr.fileno(), 4096)
            if not chunk:
                fail(
                    f"UDP relay exited before reporting {expected!r}: "
                    f"rc={process.poll()}, stderr={output!r}"
                )
            output += chunk
    finally:
        selector.close()


def start_echo_target(expected_messages):
    target = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    target.bind(("127.0.0.1", 0))
    target.settimeout(5)
    errors = []

    def echo_messages():
        try:
            for _ in range(expected_messages):
                payload, address = target.recvfrom(1024)
                target.sendto(payload, address)
        except Exception as error:
            errors.append(error)

    thread = threading.Thread(target=echo_messages)
    thread.start()
    return target, target.getsockname()[1], thread, errors


def finish_echo_target(target, thread, errors):
    thread.join(timeout=5)
    target.close()
    if thread.is_alive() or errors:
        fail(f"UDP echo target did not finish cleanly: {errors!r}")


def exchange(client, listen_port, payload):
    client.sendto(payload, ("127.0.0.1", listen_port))
    received, _ = client.recvfrom(1024)
    if received != payload:
        fail(f"UDP relay changed payload: {received!r} != {payload!r}")


def fault_environment(name=None):
    environment = os.environ.copy()
    for variable in FAULT_VARIABLES:
        environment.pop(variable, None)
    if name:
        environment[name] = "1"
    return environment


def test_send_recovery(binary, work):
    target, target_port, thread, errors = start_echo_target(1)
    try:
        with running_relay(
            binary,
            work,
            target_port,
            fault_environment("HAMN_TEST_UDP_SEND_FAILURE"),
        ) as (process, listen_port), socket.socket(
            socket.AF_INET, socket.SOCK_DGRAM
        ) as client:
            client.settimeout(5)
            client.sendto(
                b"injected-send-failure", ("127.0.0.1", listen_port)
            )
            wait_for_stderr(process, b"UDP relay cannot send to target")
            exchange(client, listen_port, b"send-recovered")
    finally:
        finish_echo_target(target, thread, errors)


def test_recv_recovery(binary, work):
    target, target_port, thread, errors = start_echo_target(2)
    try:
        with running_relay(
            binary,
            work,
            target_port,
            fault_environment("HAMN_TEST_UDP_RECV_FAILURE"),
        ) as (process, listen_port), socket.socket(
            socket.AF_INET, socket.SOCK_DGRAM
        ) as client:
            client.settimeout(5)
            client.sendto(
                b"injected-recv-failure", ("127.0.0.1", listen_port)
            )
            wait_for_stderr(process, b"UDP relay cannot receive from target")
            exchange(client, listen_port, b"recv-recovered")
    finally:
        finish_echo_target(target, thread, errors)


def test_poll_failure(binary, work):
    listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.set_inheritable(True)
    pidfile = work / "poll-failure.pid"
    process = subprocess.Popen(
        [
            binary,
            "udp-forward",
            "--listen-address",
            "127.0.0.1",
            "--listen-port",
            str(port),
            "--listen-fd",
            str(listener.fileno()),
            "--target-address",
            "127.0.0.1",
            "--target-port",
            str(port),
            "--pidfile",
            str(pidfile),
        ],
        pass_fds=(listener.fileno(),),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=fault_environment("HAMN_TEST_UDP_POLL_FAILURE"),
    )
    listener.close()
    try:
        _, stderr = process.communicate(timeout=5)
        if process.returncode == 0 or b"UDP relay poll failed" not in stderr:
            fail(f"injected poll failure was hidden: rc={process.returncode}")
        if pidfile.exists():
            fail("poll failure left a UDP relay pidfile")
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()

    target, target_port, thread, errors = start_echo_target(1)
    try:
        with running_relay(binary, work, target_port) as (_, listen_port), \
                socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
            client.settimeout(5)
            exchange(client, listen_port, b"poll-recovered")
    finally:
        finish_echo_target(target, thread, errors)


def test_flow_limit_and_eviction(binary, work):
    target = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    target.bind(("127.0.0.1", 0))
    target.settimeout(5)
    clients = []
    target_addresses = []
    try:
        with running_relay(
            binary, work, target.getsockname()[1]
        ) as (process, listen_port):
            for index in range(FLOW_LIMIT + 1):
                client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                client.bind(("127.0.0.1", 0))
                client.setblocking(False)
                clients.append(client)
                payload = f"open-{index}".encode()
                client.sendto(payload, ("127.0.0.1", listen_port))
                received, target_address = target.recvfrom(1024)
                if received != payload:
                    fail(
                        f"flow {index} opened with another payload: "
                        f"{received!r}"
                    )
                target_addresses.append(target_address)

            process.send_signal(signal.SIGSTOP)
            stopped_pid, status = os.waitpid(process.pid, os.WUNTRACED)
            if stopped_pid != process.pid or not os.WIFSTOPPED(status):
                fail("UDP relay did not enter the deterministic test barrier")
            for index, target_address in enumerate(target_addresses):
                target.sendto(
                    f"reply-{index}".encode(), target_address
                )

            process.send_signal(signal.SIGCONT)
            clients[-1].sendto(b"barrier", ("127.0.0.1", listen_port))
            received, barrier_address = target.recvfrom(1024)
            if received != b"barrier":
                fail(f"flow barrier received another payload: {received!r}")
            target.sendto(b"barrier-complete", barrier_address)

            selector = selectors.DefaultSelector()
            responses = [[] for _ in clients]
            selector.register(process.stdout, selectors.EVENT_READ, None)
            for index, client in enumerate(clients):
                selector.register(client, selectors.EVENT_READ, index)
            deadline = time.monotonic() + 5
            barrier_seen = False
            try:
                while not barrier_seen:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        fail("UDP flow boundary barrier timed out")
                    events = selector.select(remaining)
                    if not events:
                        fail("UDP flow boundary barrier timed out")
                    for key, _ in events:
                        if key.data is None:
                            fail("UDP relay exited during flow boundary test")
                        while True:
                            try:
                                payload, _ = key.fileobj.recvfrom(1024)
                            except BlockingIOError:
                                break
                            responses[key.data].append(payload)
                            if payload == b"barrier-complete":
                                barrier_seen = True
                for key, _ in selector.select(0):
                    if key.data is not None:
                        try:
                            payload, _ = key.fileobj.recvfrom(1024)
                            responses[key.data].append(payload)
                        except BlockingIOError:
                            pass
            finally:
                selector.close()

            live_old_flows = 0
            for index, response in enumerate(responses):
                expected = f"reply-{index}".encode()
                if index == FLOW_LIMIT:
                    if response != [expected, b"barrier-complete"]:
                        fail(f"new flow received mixed responses: {response!r}")
                elif response == [expected]:
                    live_old_flows += 1
                elif response:
                    fail(f"existing flow {index} received mixed responses: {response!r}")
            if live_old_flows != FLOW_LIMIT - 1:
                fail(
                    f"65th flow left {live_old_flows} existing flows active; "
                    f"expected {FLOW_LIMIT - 1}"
                )
    finally:
        for client in clients:
            client.close()
        target.close()


def main():
    production = len(sys.argv) == 4 and sys.argv[3] == "--production"
    if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and not production):
        fail("usage: test_udp_proxy.py TEST_BINARY WORK_DIRECTORY [--production]")
    work = Path(sys.argv[2])
    work.mkdir(parents=True)
    if not production:
        test_send_recovery(sys.argv[1], work)
        test_recv_recovery(sys.argv[1], work)
        test_poll_failure(sys.argv[1], work)
    test_flow_limit_and_eviction(sys.argv[1], work)
    if production:
        print("PASS: production UDP relay isolates its 64-flow boundary")
    else:
        print(
            "PASS: UDP relay recovers faults and isolates its 64-flow boundary"
        )


if __name__ == "__main__":
    main()
