#!/usr/bin/env python3
"""Observation-fixture checks only: no VM, user config, or release network."""
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import threading
import unittest

from workspace_live_external_contexts import Proxy, birth, certificates, gone, wrapper


class ObservationFixtures(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix='hamn-context-observers-', dir='/tmp')
        cls.addClassCleanup(cls.temp.cleanup)
        cls.root = Path(cls.temp.name)
        cls.server_context = certificates(cls.root)

    def client(self, authority='ca', certificate=True):
        context = ssl.create_default_context(cafile=self.root / (authority + '.pem'))
        if certificate:
            context.load_cert_chain(self.root / 'client.pem', self.root / 'client.key')
        return context

    def exchange(self, port, context):
        with socket.create_connection(('127.0.0.1', port), timeout=5) as raw:
            with context.wrap_socket(raw, server_hostname='127.0.0.1') as secure:
                secure.sendall(b'owned-request\n')
                output = bytearray()
                while data := secure.recv(65536):
                    output.extend(data)
                return bytes(output)

    def test_mtls_forwards_exact_bytes_and_rejects_wrong_ca_and_missing_client(self):
        target = self.root / 'engine.sock'
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(target)); listener.listen(); listener.settimeout(5)
        data = b'Engine response ' * 65536
        received = []

        def engine():
            with listener.accept()[0] as connection:
                received.append(connection.recv(1024))
                connection.sendall(data)

        worker = threading.Thread(target=engine)
        proxy = Proxy(target, self.server_context)
        worker.start()
        try:
            self.assertEqual(self.exchange(proxy.port, self.client()), data)
            worker.join(5)
            self.assertFalse(worker.is_alive())
            self.assertEqual(received, [b'owned-request\n'])
            for context in (self.client('wrong-ca'), self.client(certificate=False)):
                with self.assertRaises((ssl.SSLError, ConnectionResetError)):
                    self.exchange(proxy.port, context)
            self.assertEqual(proxy.forwarded, 1)
        finally:
            proxy.close(); listener.close(); target.unlink()
        self.assertEqual(proxy.tls_failures, 2)

    def test_stall_has_a_synchronized_entry_and_releases_owned_threads(self):
        proxy = Proxy(self.root / 'must-not-be-contacted', self.server_context)
        proxy.stall = True
        received, errors = [], []

        def request():
            try:
                received.append(self.exchange(proxy.port, self.client()))
            except Exception as error:
                errors.append(error)

        worker = threading.Thread(target=request)
        worker.start()
        try:
            self.assertTrue(proxy.entered.wait(5))
            self.assertTrue(worker.is_alive())
            self.assertEqual(proxy.forwarded, 0)
            proxy.release.set()
            worker.join(5)
            self.assertFalse(worker.is_alive())
            self.assertEqual(received, [b''])
            self.assertFalse(errors, errors)
        finally:
            proxy.close()

    def test_wrapper_executes_real_program_and_observes_its_exact_birth(self):
        witness, executable = self.root / 'births.jsonl', self.root / 'program'
        wrapper(executable, '/bin/cat', [], witness)
        child = subprocess.Popen([executable], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            # The witness is written before exec; reading the actual echoed
            # bytes also proves the wrapper has reached the real executable.
            child.stdin.write(b'observed\n'); child.stdin.flush()
            self.assertEqual(child.stdout.readline(), b'observed\n')
            identity = json.loads(witness.read_text())
            self.assertEqual(identity, birth(child.pid))
            with self.assertRaisesRegex(AssertionError, 'survived'):
                gone([identity], timeout=0)
            child.stdin.close()
            self.assertEqual(child.wait(timeout=5), 0)
            gone([identity])
        finally:
            if child.poll() is None:
                child.kill(); child.wait(timeout=5)
            child.stdout.close()


if __name__ == '__main__':
    unittest.main()
